import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAuthStore } from "../../stores/authStore";
import { stringToGradient } from "../../utils/colors";
import StreamViewPanel from "./StreamViewPanel";
import StreamVideoPlayer from "./StreamVideoPlayer";
import CaptureSourcePicker from "./CaptureSourcePicker";
import { CodecBadge } from "./CodecBadge";
import { useCodecSettingsStore } from "../../stores/codecSettingsStore";
import { canWatchStream } from "../../utils/canWatchStream";
import { useStreamThumbnails } from "./useStreamThumbnails";

const EMPTY_CHANNELS: never[] = [];

export default function VoicePanel() {
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const participants = useVoiceStore((s) => s.participants);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const streamThumbnails = useVoiceStore((s) => s.streamThumbnails);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const watchingStreams = useVoiceStore((s) => s.watchingStreams);
  const fullscreenStream = useVoiceStore((s) => s.fullscreenStream);
  const isStreamFullscreen = useVoiceStore((s) => s.isStreamFullscreen);
  const isStreaming = useVoiceStore((s) => s.isStreaming);
  const disconnect = useVoiceStore((s) => s.disconnect);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  const channels = useChatStore((s) => {
    const serverId = s.activeServerId;
    return serverId ? s.channelsByServer[serverId] ?? EMPTY_CHANNELS : EMPTY_CHANNELS;
  });

  const ownUsername = useAuthStore((s) => s.username);

  const [showPicker, setShowPicker] = useState(false);

  useStreamThumbnails();

  const channelName =
    channels.find((ch) => ch.id === connectedChannelId)?.name ?? "Voice";

  const hasStreams = activeStreams.length > 0;

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

  const handleWatchStream = (username: string) => {
    if (!connectedServerId || !connectedChannelId) return;
    const isSelf = username === ownUsername;
    const isAlreadyWatching = watchingStreams.includes(username);
    if (!isAlreadyWatching) {
      if (isSelf) {
        invoke("watch_self_stream", { enabled: true }).catch(() => {});
      } else {
        invoke("watch_stream", {
          serverId: connectedServerId,
          channelId: connectedChannelId,
          targetUsername: username,
        }).catch(() => {});
      }
      useVoiceStore.getState().addWatching(username);
    }
    useVoiceStore.getState().setFullscreenStream(username);
  };

  const handleStopSharing = () => {
    invoke("stop_screen_share", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
    }).catch(console.error);
    useVoiceStore.getState().setIsStreaming(false);
  };

  const handleDisconnect = async () => {
    if (connectedServerId && connectedChannelId) {
      for (const username of watchingStreams) {
        if (username === ownUsername) {
          await invoke("watch_self_stream", { enabled: false }).catch(() => {});
        } else {
          await invoke("stop_watching", {
            serverId: connectedServerId,
            channelId: connectedChannelId,
            targetUsername: username,
          }).catch(() => {});
        }
      }
    }
    invoke("leave_voice_channel").catch(console.error);
    disconnect();
    setActiveView("server");
  };

  return (
    <div className="flex min-h-0 flex-1 flex-col bg-bg-mid">
      {/* Header — h-12 / px-4 / border-border to match the server-name
          header in ChannelSidebar so the two line up across the divider. */}
      {!isStreamFullscreen && (
        <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border px-4">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="text-text-muted">
            <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/>
            <path d="M19 10v2a7 7 0 0 1-14 0v-2"/>
          </svg>
          <span className="font-display text-[15px] font-semibold text-text-primary">{channelName}</span>
          <span
            className="ml-auto text-[12px] text-text-muted"
            title={latencyMs != null ? `${latencyMs}ms` : undefined}
          >
            {participants.length} participant{participants.length !== 1 ? "s" : ""}
            {latencyMs != null && (
              <span className={`ml-2 font-medium ${
                latencyMs <= 70 ? "text-success" : latencyMs < 175 ? "text-warning" : "text-error"
              }`}>{latencyMs}ms</span>
            )}
          </span>
        </div>
      )}

      {/* StreamViewPanel */}
      {watchingStreams.length > 0 && (
        <div className={fullscreenStream ? "flex min-h-0 flex-1" : "hidden"}>
          <StreamViewPanel />
        </div>
      )}

      {/* Stream cards */}
      {hasStreams && (
        <div className={fullscreenStream ? "hidden" : "flex flex-1 overflow-hidden"}>
          <div className="flex flex-1 flex-col overflow-hidden">
            <div className="px-5 py-3 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
              Live — {activeStreams.length}
            </div>
            {/* pt-4 gives the lifted-card hover state (transform + accent glow
                shadow) headroom so the top edge doesn't clip against the
                scroll container's overflow boundary. */}
            <div className="flex-1 overflow-y-auto px-5 pt-4 pb-4">
              <div className="grid gap-4" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))" }}>
                {activeStreams.map((stream) => {
                  const isWatching = watchingStreams.includes(stream.ownerUsername);
                  const thumbnail = streamThumbnails[stream.ownerUsername];
                  const decodeCaps = useCodecSettingsStore.getState().decodeCaps;
                  const isOwnStream = stream.ownerUsername === ownUsername;
                  const { canWatch, reason } = isOwnStream
                    ? { canWatch: true, reason: undefined }
                    : canWatchStream(stream, decodeCaps);
                  return (
                    <button
                      key={stream.streamId}
                      disabled={!canWatch}
                      title={reason}
                      onClick={() => canWatch && handleWatchStream(stream.ownerUsername)}
                      className={`group relative overflow-hidden rounded-xl border transition-all duration-200 ease-out ${
                        !canWatch
                          ? "cursor-not-allowed border-border-divider opacity-50"
                          : isWatching
                          ? "border-accent/40 shadow-[0_0_12px_var(--color-accent-soft)] hover:-translate-y-0.5 hover:shadow-[0_8px_20px_rgba(0,0,0,0.35),0_0_16px_var(--color-accent-soft)]"
                          : "border-border bg-bg-light hover:-translate-y-0.5 hover:border-accent/30 hover:shadow-[0_8px_20px_rgba(0,0,0,0.35)]"
                      }`}
                    >
                      <div className="relative aspect-video w-full bg-bg-darkest">
                        <CodecBadge
                          codec={stream.currentCodec}
                          width={stream.resolutionWidth}
                          height={stream.resolutionHeight}
                          fps={stream.fps}
                          enforced={stream.enforcedCodec !== 0}
                          size="small"
                        />
                        {isWatching ? (
                          <StreamVideoPlayer
                            streamerUsername={stream.ownerUsername}
                            className="h-full w-full object-cover"
                          />
                        ) : thumbnail ? (
                          <img
                            src={thumbnail}
                            alt={`${stream.ownerUsername}'s stream`}
                            className="h-full w-full object-cover"
                          />
                        ) : (
                          <div className="flex h-full w-full items-center justify-center">
                            <div
                              className="flex h-14 w-14 items-center justify-center rounded-lg text-2xl font-bold text-white"
                              style={{ background: stringToGradient(stream.ownerUsername) }}
                            >
                              {stream.ownerUsername.charAt(0).toUpperCase()}
                            </div>
                          </div>
                        )}
                        {/* Live / Watching badge */}
                        <div className={`absolute left-2.5 top-2.5 flex items-center gap-[5px] rounded-md px-2 py-1 ${
                          isWatching ? "bg-accent/90" : "bg-error/90"
                        }`}>
                          <div className="h-1.5 w-1.5 animate-pulse rounded-full bg-white" />
                          <span className="text-[10px] font-semibold text-white">
                            {isWatching ? "WATCHING" : "LIVE"}
                          </span>
                        </div>
                        {/* Hover overlay */}
                        <div className="absolute inset-0 flex items-center justify-center bg-black/0 transition-colors group-hover:bg-black/40">
                          <span className="text-[13px] font-medium text-white opacity-0 transition-opacity group-hover:opacity-100">
                            {isWatching ? "Expand" : "Watch Stream"}
                          </span>
                        </div>
                      </div>
                      {/* Stream info */}
                      <div className="flex items-center gap-2.5 px-3.5 py-3">
                        <div
                          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-[12px] font-semibold text-white"
                          style={{ background: stringToGradient(stream.ownerUsername) }}
                        >
                          {stream.ownerUsername.charAt(0).toUpperCase()}
                        </div>
                        <div className="min-w-0 flex-1 text-left">
                          <div className="flex items-center gap-1.5">
                            <span className="truncate text-[13px] font-medium text-text-primary">
                              {stream.ownerUsername}
                            </span>
                            {stream.hasAudio && (
                              <svg className="h-3.5 w-3.5 shrink-0 text-accent-bright" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
                                <path d="M11 5L6 9H2v6h4l5 4V5z" />
                                <path d="M15.54 8.46a5 5 0 010 7.07" />
                              </svg>
                            )}
                          </div>
                          <div className="text-[11px] text-text-muted">
                            {stream.resolutionWidth > 0
                              ? `${stream.resolutionWidth}x${stream.resolutionHeight}`
                              : ""}
                            {stream.fps > 0 ? ` · ${stream.fps}fps` : ""}
                          </div>
                        </div>
                        {isWatching && (
                          <button
                            onClick={(e) => {
                              e.stopPropagation();
                              if (stream.ownerUsername === ownUsername) {
                                invoke("watch_self_stream", { enabled: false }).catch(() => {});
                              } else {
                                invoke("stop_watching", {
                                  serverId: connectedServerId,
                                  channelId: connectedChannelId,
                                  targetUsername: stream.ownerUsername,
                                }).catch(() => {});
                              }
                              useVoiceStore.getState().removeWatching(stream.ownerUsername);
                            }}
                            className="ml-auto flex h-7 items-center gap-1.5 rounded-md border border-error/[0.25] bg-error/[0.12] px-2.5 text-[11px] font-medium text-error transition-colors hover:border-error/[0.4] hover:bg-error/[0.18]"
                          >
                            <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                              <rect x="2" y="3" width="20" height="14" rx="2" />
                              <line x1="8" y1="21" x2="16" y2="21" />
                              <line x1="12" y1="17" x2="12" y2="21" />
                              <line x1="7" y1="7" x2="17" y2="13" />
                              <line x1="17" y1="7" x2="7" y2="13" />
                            </svg>
                            Stop Watching
                          </button>
                        )}
                      </div>
                    </button>
                  );
                })}
              </div>
            </div>
          </div>
        </div>
      )}

      {/* No streams: centered participant grid */}
      {!fullscreenStream && !hasStreams && (
        <div className="flex flex-1 flex-wrap items-center justify-center gap-5 p-6">
          {participants.map((p) => {
            const isSpeaking = speakingUsers.includes(p.username);
            return (
              <div
                key={p.username}
                className="flex cursor-pointer flex-col items-center gap-2.5 rounded-xl px-5 py-4 transition-all hover:bg-white/[0.035]"
                onClick={(e) => {
                  const rect = e.currentTarget.getBoundingClientRect();
                  openProfilePopup(p.username, { x: rect.right + 8, y: rect.top }, connectedServerId);
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  openContextMenu(p.username, { x: e.clientX, y: e.clientY });
                }}
              >
                <div className="relative">
                  <div
                    className={`flex h-20 w-20 items-center justify-center rounded-xl text-[28px] font-bold text-white transition-all duration-200 ${
                      isSpeaking ? "shadow-[0_0_0_3px_var(--color-bg-mid),0_0_0_5px_var(--color-success)]" : ""
                    }`}
                    style={{ background: stringToGradient(p.username) }}
                  >
                    {p.username.charAt(0).toUpperCase()}
                  </div>
                  {p.isMuted && (
                    <div className="absolute -bottom-1 -right-1 flex h-[22px] w-[22px] items-center justify-center rounded-full border-[2.5px] border-bg-mid bg-bg-light">
                      <svg className="h-3 w-3 text-error" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                        <line x1="1" y1="1" x2="23" y2="23" />
                        <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
                        <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
                        <line x1="12" y1="19" x2="12" y2="23" />
                        <line x1="8" y1="23" x2="16" y2="23" />
                      </svg>
                    </div>
                  )}
                </div>
                <div className="max-w-full truncate text-center text-[13px] font-medium text-text-primary">
                  {p.username}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Bottom controls */}
      {!isStreamFullscreen && (
        <div className="flex justify-center gap-2 border-t border-border-divider bg-bg-dark px-5 py-3.5">
          <button
            onClick={handleMute}
            className={`flex items-center gap-[7px] rounded-[10px] border px-[18px] py-[9px] text-[13px] font-medium transition-colors ${
              isMuted
                ? "border-error/20 bg-error/10 text-error"
                : "border-border bg-bg-light text-text-secondary hover:bg-bg-lighter hover:text-text-primary"
            }`}
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
            {isMuted ? "Unmute" : "Mute"}
          </button>
          <button
            onClick={handleDeafen}
            className={`flex items-center gap-[7px] rounded-[10px] border px-[18px] py-[9px] text-[13px] font-medium transition-colors ${
              isDeafened
                ? "border-error/20 bg-error/10 text-error"
                : "border-border bg-bg-light text-text-secondary hover:bg-bg-lighter hover:text-text-primary"
            }`}
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
            {isDeafened ? "Undeafen" : "Deafen"}
          </button>
          <button
            onClick={isStreaming ? handleStopSharing : () => setShowPicker(true)}
            className={`flex items-center gap-[7px] rounded-[10px] border px-[18px] py-[9px] text-[13px] font-medium transition-colors ${
              isStreaming
                ? "border-accent/25 bg-accent/[0.12] text-accent hover:bg-accent/[0.18]"
                : "border-accent/20 bg-accent-soft text-accent hover:bg-accent/[0.18] hover:text-accent-bright"
            }`}
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
            className="flex items-center gap-[7px] rounded-[10px] border border-error/20 bg-error/10 px-[18px] py-[9px] text-[13px] font-medium text-error transition-colors hover:bg-error/[0.18]"
          >
            Disconnect
          </button>
        </div>
      )}
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
