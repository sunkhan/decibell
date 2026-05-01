import { useState, useEffect, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { stringToGradient } from "../../utils/colors";
import CaptureSourcePicker from "../voice/CaptureSourcePicker";
import ConnectionStatsPopover from "../voice/ConnectionStatsPopover";
import DeviceContextMenu from "../voice/DeviceContextMenu";
import { playSound } from "../../utils/sounds";

const EMPTY_CHANNELS: never[] = [];

export default function UserPanel() {
  const username = useAuthStore((s) => s.username);
  const openModal = useUiStore((s) => s.openModal);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const isSpeaking = username ? speakingUsers.includes(username) : false;

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
  const [showStats, setShowStats] = useState(false);
  const [deviceMenu, setDeviceMenu] = useState<{
    type: "input" | "output";
    anchor: { x: number; y: number };
  } | null>(null);
  const [cachedDevices, setCachedDevices] = useState<{
    inputs: { name: string; label?: string }[];
    outputs: { name: string; label?: string }[];
  }>({ inputs: [], outputs: [] });

  const refreshDevices = useCallback(() => {
    invoke<{ inputs: { name: string; label?: string }[]; outputs: { name: string; label?: string }[] }>("list_audio_devices")
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
      playSound("undeafen");
      invoke("set_voice_deafen", { deafened: false }).catch(console.error);
      invoke("set_voice_mute", { muted: false }).catch(console.error);
    } else {
      playSound(isMuted ? "unmute" : "mute");
      invoke("set_voice_mute", { muted: !isMuted }).catch(console.error);
    }
  };

  const handleDeafen = () => {
    playSound(isDeafened ? "undeafen" : "deafen");
    invoke("set_voice_deafen", { deafened: !isDeafened }).catch(console.error);
  };

  const handleDisconnect = () => {
    playSound("disconnect");
    invoke("leave_voice_channel").catch(console.error);
    disconnect();
    setActiveView("server");
  };

  const handleStopSharing = () => {
    playSound("stream_stop");
    invoke("stop_screen_share", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
    }).catch(console.error);
    useVoiceStore.getState().setIsStreaming(false);
  };

  return (
    <div className="rounded-[14px] border border-border bg-bg-light px-3 py-2.5 shadow-[0_4px_16px_rgba(0,0,0,0.5),0_0_0_1px_rgba(255,255,255,0.04)]">
      {/* Voice connection info */}
      {connectedChannelId && (
        <div className="mb-2 flex items-center gap-1.5 px-0.5">
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="text-text-muted">
            <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/>
            <path d="M19 10v2a7 7 0 0 1-14 0v-2"/>
          </svg>
          <span className="font-display text-[13px] font-semibold text-text-primary">{channelName}</span>
          {activeStreams.length > 0 && (
            <span className="rounded bg-accent/[0.12] px-1.5 py-0.5 text-[10px] font-semibold text-accent">
              {activeStreams.length} stream{activeStreams.length > 1 ? "s" : ""}
            </span>
          )}
          <div className="relative ml-auto">
            <button
              onClick={() => setShowStats((v) => !v)}
              className={`text-[11px] font-medium hover:underline focus:outline-none ${
                latencyMs != null
                  ? latencyMs <= 70 ? "text-success" : latencyMs < 175 ? "text-warning" : "text-error"
                  : "text-success"
              }`}
              title="Click for connection stats"
            >
              {latencyMs != null ? `${latencyMs}ms` : "Connected"}
            </button>
            {showStats && (
              <ConnectionStatsPopover onClose={() => setShowStats(false)} />
            )}
          </div>
        </div>
      )}
      {error && (
        <p className="mb-1.5 px-0.5 text-[10px] text-warning">{error}</p>
      )}

      {/* Voice action row */}
      {connectedChannelId && (
        <div className="mb-2 flex items-center gap-1.5">
          <button
            onClick={isStreaming ? handleStopSharing : () => setShowPicker(true)}
            className={`flex h-8 flex-1 items-center justify-center gap-[6px] rounded-lg border text-[12px] font-medium transition-colors ${
              isStreaming
                ? "border-accent/[0.25] bg-accent/[0.12] text-accent hover:bg-accent/[0.18]"
                : "border-accent/[0.2] bg-accent/[0.08] text-accent hover:bg-accent/[0.15] hover:text-accent-bright"
            }`}
            title={isStreaming ? "Stop sharing" : "Share screen"}
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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
            className="flex h-8 flex-1 items-center justify-center gap-[6px] rounded-lg border border-error/[0.25] bg-error/[0.12] text-[12px] font-medium text-error transition-colors hover:border-error/[0.4] hover:bg-error/[0.18]"
            title="Disconnect"
          >
            Disconnect
          </button>
        </div>
      )}

      {/* User info + controls row */}
      <div className="flex items-center gap-2.5">
        {/* Avatar */}
        <div className="relative shrink-0">
          <div
            className="flex h-[34px] w-[34px] items-center justify-center rounded-lg text-sm font-semibold text-white transition-shadow duration-200"
            style={{
              background: stringToGradient(username),
              boxShadow: isSpeaking
                ? "0 0 0 2px var(--color-bg-light), 0 0 0 4px var(--color-success)"
                : "none",
            }}
          >
            {username.charAt(0).toUpperCase()}
          </div>
          <div className="absolute -bottom-px -right-px h-3 w-3 rounded-full border-[2.5px] border-bg-light bg-success" />
        </div>

        {/* Username */}
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-medium text-text-primary">
            {username}
          </div>
          <div className="text-[11px] text-success">Online</div>
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
            className={`flex h-[36px] w-[36px] items-center justify-center rounded-lg transition-colors ${
              isMuted
                ? "bg-error/[0.15] text-error"
                : "text-text-muted hover:bg-surface-hover hover:text-text-secondary"
            }`}
            title={isMuted ? "Unmute" : "Mute — Right-click to change input device"}
          >
            <svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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
            className={`flex h-[36px] w-[36px] items-center justify-center rounded-lg transition-colors ${
              isDeafened
                ? "bg-error/[0.15] text-error"
                : "text-text-muted hover:bg-surface-hover hover:text-text-secondary"
            }`}
            title={isDeafened ? "Undeafen" : "Deafen — Right-click to change output device"}
          >
            <svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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
            className="flex h-[36px] w-[36px] items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
            title="Settings"
          >
            <svg width="19" height="19" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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
