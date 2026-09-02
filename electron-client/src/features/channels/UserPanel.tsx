import { useState } from "react";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUpdateStore } from "../../stores/updateStore";
import { useAudioDevicesStore } from "../../stores/audioDevicesStore";
import { UserAvatar } from "../../components/UserAvatar";
import { playSound } from "../../utils/sounds";
import DeviceContextMenu from "../voice/DeviceContextMenu";
import ConnectionStatsPopover from "../voice/ConnectionStatsPopover";
import CaptureSourcePicker from "../voice/CaptureSourcePicker";
import { StreamAudioButton } from "../voice/StreamAudioPopover";
import { announceCallStreamStop, endCall } from "../call/callActions";

const EMPTY_CHANNELS: never[] = [];

// Floating user-control panel that sits at the bottom-left of the
// sidebar group (over ChannelSidebar). Shows the user's
// avatar + username, voice status when connected, and the standard
// quick-action row (mute / deafen / disconnect / device pickers).
//
// Streaming controls (start sharing, capture picker, stop sharing)
// landed with PR8. The settings cog opens SettingsModal (mounted in
// MainLayout) via uiStore.openModal("settings").
export default function UserPanel() {
  const username = useAuthStore((s) => s.username);
  const openModal = useUiStore((s) => s.openModal);
  // Subscribe to a derived boolean instead of the whole Set so this
  // panel only re-renders when *our* speaking state changes — every
  // other user's speaking events become no-ops here.
  const isSpeaking = useVoiceStore((s) =>
    username ? s.speakingUsers.has(username) : false,
  );
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  // P2P DM call: same engine, no channel. Gates below use `inSession`.
  const callPeer = useVoiceStore((s) => s.callPeer);
  const inSession = connectedChannelId != null || callPeer != null;
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
  const updateStatus = useUpdateStore((s) => s.status);
  const updateMode = useUpdateStore((s) => s.mode);
  const showChip =
    updateStatus.state === "downloaded" && updateMode === "self-update";
  const handleChipRestart = () => {
    window.decibell.update.quitAndInstall().catch((err) => {
      console.error("[update] quitAndInstall failed:", err);
    });
  };

  const [showStats, setShowStats] = useState(false);
  const [showPicker, setShowPicker] = useState(false);
  const [deviceMenu, setDeviceMenu] = useState<{
    type: "input" | "output";
    anchor: { x: number; y: number };
  } | null>(null);
  // Device roster comes from the shared store, kept fresh by the app-global
  // devicechange sync — so the right-click device menu reflects hotplugs
  // instead of a stale once-per-mount snapshot.
  const deviceInputs = useAudioDevicesStore((s) => s.inputs);
  const deviceOutputs = useAudioDevicesStore((s) => s.outputs);

  if (!username) return null;

  const channelName = callPeer
    ? `Call · ${callPeer}`
    : connectedChannelId
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

  const handleDisconnect = async () => {
    playSound("disconnect");
    // If streaming, stop the capture/encoder + native stream before
    // leaving; otherwise capture keeps running after disconnect with no
    // UI left to stop it.
    if (useVoiceStore.getState().isStreaming) {
      const { stopActiveStream } = await import(
        "../voice/streaming/StreamCapture"
      );
      await stopActiveStream();
      await invoke("stop_screen_share", {
        serverId: connectedServerId ?? undefined,
        channelId: connectedChannelId ?? undefined,
      }).catch(console.error);
      useVoiceStore.getState().setIsStreaming(false);
    }
    invoke("leave_voice_channel").catch(console.error);
    disconnect();
    setActiveView("server");
  };

  const handleHangUp = () => {
    void endCall("Call ended");
  };

  const handleStopSharing = async () => {
    playSound("stream_stop");
    // PR8: tear down the renderer-side capture + encoder first so no
    // more frames are pushed to native after we tell native to stop.
    const { stopActiveStream } = await import(
      "../voice/streaming/StreamCapture"
    );
    await stopActiveStream();
    invoke("stop_screen_share", {
      serverId: connectedServerId ?? undefined,
      channelId: connectedChannelId ?? undefined,
    }).catch(console.error);
    useVoiceStore.getState().setIsStreaming(false);
    announceCallStreamStop();
  };

  const openDeviceMenu = (type: "input" | "output", e: React.MouseEvent) => {
    const rect = e.currentTarget.getBoundingClientRect();
    setDeviceMenu({ type, anchor: { x: rect.left, y: rect.top } });
  };

  return (
    <div className="rounded-lg border border-border bg-bg-light px-3 py-2.5 shadow-float">
      {showChip && (
        <button
          onClick={handleChipRestart}
          title={`Restart to update to ${updateStatus.state === "downloaded" ? updateStatus.version : ""}`}
          className="mb-2 flex w-full items-center gap-2 rounded-sm bg-accent-soft px-2 py-1.5 text-left text-[12px] font-medium text-accent-bright transition-colors hover:bg-accent-mid"
        >
          <span className="h-1.5 w-1.5 rounded-full bg-accent-bright animate-[dropPulse_2.4s_ease-in-out_infinite]" />
          <span className="min-w-0 flex-1 truncate">
            Update ready
            {updateStatus.state === "downloaded" ? ` — ${updateStatus.version}` : ""}
          </span>
          <span className="shrink-0 font-meta text-micro font-semibold uppercase tracking-[0.1em]">
            Restart
          </span>
        </button>
      )}
      {inSession && (
        <div className="mb-2 flex items-center gap-1.5 px-0.5">
          {callPeer ? (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="text-text-muted">
              <path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92z" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="text-text-muted">
              <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
              <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
            </svg>
          )}
          <span className="font-display text-member font-emphasis text-text-primary">{channelName}</span>
          {activeStreams.length > 0 && (
            <span className="rounded-sm bg-accent/[0.12] px-1.5 py-0.5 text-[10px] font-semibold text-accent">
              {activeStreams.length} stream{activeStreams.length > 1 ? "s" : ""}
            </span>
          )}
          <div className="relative ml-auto">
            <button
              onClick={() => setShowStats((v) => !v)}
              className={`text-[11px] font-medium hover:underline focus:outline-none ${
                latencyMs != null
                  ? latencyMs <= 70
                    ? "text-success"
                    : latencyMs < 175
                    ? "text-warning"
                    : "text-error"
                  : "text-success"
              }`}
              title="Click for connection stats"
            >
              {latencyMs != null ? `${latencyMs}ms` : "Connected"}
            </button>
            {showStats && <ConnectionStatsPopover onClose={() => setShowStats(false)} />}
          </div>
        </div>
      )}
      {error && inSession && (
        <div className="mb-2 rounded-sm bg-error/10 px-2 py-1 text-[11px] text-error">{error}</div>
      )}

      {inSession && !callPeer && (
        <div className="mb-2 flex items-center gap-1.5">
          <button
            onClick={isStreaming ? handleStopSharing : () => setShowPicker(true)}
            className={`flex h-8 flex-1 items-center justify-center gap-[6px] rounded-md border text-[12px] font-medium transition-colors ${
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
          <StreamAudioButton
            size={13}
            className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-accent/[0.2] bg-accent/[0.08] text-accent transition-colors hover:bg-accent/[0.15] hover:text-accent-bright"
          />
        </div>
      )}

      <div className="flex items-center gap-2">
        <div
          className="relative shrink-0 rounded-md transition-shadow"
          style={{
            boxShadow: isSpeaking ? "0 0 0 2px var(--color-success)" : "none",
          }}
        >
          <UserAvatar username={username} size={36} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="truncate font-channel text-member font-emphasis text-text-bright">
            {username}
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-1">
          {inSession && (
            <PanelButton
              title={callPeer ? "Hang up" : "Disconnect"}
              onClick={callPeer ? handleHangUp : handleDisconnect}
              variant="danger"
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <path d="M16 17l5-5-5-5" />
                <path d="M21 12H9" />
                <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
              </svg>
            </PanelButton>
          )}
          <PanelButton
            title={isMuted ? "Unmute" : "Mute"}
            onClick={handleMute}
            onContextMenu={(e) => {
              e.preventDefault();
              openDeviceMenu("input", e);
            }}
            active={isMuted}
            variant={isMuted ? "danger" : "default"}
          >
            {isMuted ? (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <line x1="1" y1="1" x2="23" y2="23" />
                <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
                <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
                <line x1="12" y1="19" x2="12" y2="23" />
                <line x1="8" y1="23" x2="16" y2="23" />
              </svg>
            ) : (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
                <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
                <line x1="12" y1="19" x2="12" y2="23" />
                <line x1="8" y1="23" x2="16" y2="23" />
              </svg>
            )}
          </PanelButton>
          <PanelButton
            title={isDeafened ? "Undeafen" : "Deafen"}
            onClick={handleDeafen}
            onContextMenu={(e) => {
              e.preventDefault();
              openDeviceMenu("output", e);
            }}
            active={isDeafened}
            variant={isDeafened ? "danger" : "default"}
          >
            {isDeafened ? (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
                <line x1="1" y1="1" x2="23" y2="23" />
              </svg>
            ) : (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
              </svg>
            )}
          </PanelButton>
          <PanelButton title="Settings" onClick={() => openModal("settings")}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="3" />
              <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
            </svg>
          </PanelButton>
        </div>
      </div>

      {deviceMenu && (
        <DeviceContextMenu
          type={deviceMenu.type}
          anchor={deviceMenu.anchor}
          devices={deviceMenu.type === "input" ? deviceInputs : deviceOutputs}
          onClose={() => setDeviceMenu(null)}
        />
      )}
      {showPicker && (callPeer || (connectedServerId && connectedChannelId)) && (
        <CaptureSourcePicker
          serverId={connectedServerId ?? undefined}
          channelId={connectedChannelId ?? undefined}
          onClose={() => setShowPicker(false)}
        />
      )}
    </div>
  );
}

function PanelButton({
  title,
  onClick,
  onContextMenu,
  active,
  variant = "default",
  children,
}: {
  title: string;
  onClick: () => void;
  onContextMenu?: (e: React.MouseEvent) => void;
  active?: boolean;
  variant?: "default" | "danger";
  children: React.ReactNode;
}) {
  const base = "flex h-8 w-8 items-center justify-center rounded-sm transition-colors";
  const tone =
    variant === "danger"
      ? "bg-error/15 text-error hover:bg-error/25"
      : active
      ? "bg-accent-soft text-accent-bright"
      : "text-text-secondary hover:bg-surface-hover hover:text-text-primary";
  return (
    <button title={title} onClick={onClick} onContextMenu={onContextMenu} className={`${base} ${tone}`}>
      {children}
    </button>
  );
}
