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

  // Generate thumbnails for active streams
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
      {/* Header - hidden when stream is in real fullscreen */}
      {!isStreamFullscreen && (
        <div className="flex items-center gap-2.5 border-b border-border px-4 py-3">
          <span className="text-accent">🔊</span>
          <span className="text-sm font-bold text-text-bright">{channelName}</span>
          <span
            className="ml-auto text-xs text-text-muted"
            title={latencyMs != null ? `${latencyMs}ms` : undefined}
          >
            {participants.length} participant{participants.length !== 1 ? "s" : ""}
            {latencyMs != null && (
              <span className={`ml-2 font-semibold ${
                latencyMs <= 70 ? "text-success" : latencyMs < 175 ? "text-warning" : "text-error"
              }`}>{latencyMs}ms</span>
            )}
          </span>
        </div>
      )}

      {/* StreamViewPanel - stays mounted while watching to preserve the decoder */}
      {watchingStreams.length > 0 && (
        <div className={fullscreenStream ? "flex min-h-0 flex-1" : "hidden"}>
          <StreamViewPanel />
        </div>
      )}

      {/* Stream cards - stays mounted (hidden when expanded) to preserve card decoders */}
      {hasStreams && (
        <div className={fullscreenStream ? "hidden" : "flex flex-1 overflow-hidden"}>
          {/* NOTE: Participants list disabled — redundant with the channel sidebar.
             Kept commented out in case we want to bring it back later.
          <div className="flex flex-col border-r border-border" style={{ width: "240px", minWidth: "240px" }}>
            <div className="px-4 py-2.5 text-[11px] font-bold uppercase tracking-wider text-text-muted">
              Users — {participants.length}
            </div>
            <div className="flex-1 overflow-y-auto px-3 pb-3">
              {participants.map((p) => {
                const isSpeaking = speakingUsers.includes(p.username);
                const isStreaming = activeStreams.some((s) => s.ownerUsername === p.username);
                return (
                  <div
                    key={p.username}
                    className="flex items-center gap-2.5 rounded-lg px-2 py-1.5 hover:bg-surface-hover transition-colors"
                  >
                    <div
                      className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-full text-xs font-bold text-white transition-all ${
                        isSpeaking ? "ring-2 ring-success" : ""
                      }`}
                      style={{ background: stringToGradient(p.username) }}
                    >
                      {p.username.charAt(0).toUpperCase()}
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium text-text-primary">
                        {p.username}
                      </div>
                      {isStreaming && (
                        <div className="text-[10px] font-medium text-accent">Streaming</div>
                      )}
                    </div>
                    {p.isMuted && (
                      <svg className="h-3.5 w-3.5 shrink-0 text-error" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                        <line x1="1" y1="1" x2="23" y2="23" />
                        <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
                        <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
                        <line x1="12" y1="19" x2="12" y2="23" />
                        <line x1="8" y1="23" x2="16" y2="23" />
                      </svg>
                    )}
                  </div>
                );
              })}
            </div>
          </div>
          */}

          {/* Stream cards */}
          <div className="flex flex-1 flex-col overflow-hidden">
            <div className="px-4 py-2.5 text-[11px] font-bold uppercase tracking-wider text-text-muted">
              Live — {activeStreams.length}
            </div>
            <div className="flex-1 overflow-y-auto px-4 pb-4">
              <div className="grid gap-4" style={{ gridTemplateColumns: "repeat(auto-fill, minmax(280px, 1fr))" }}>
                {activeStreams.map((stream) => {
                  const isWatching = watchingStreams.includes(stream.ownerUsername);
                  const thumbnail = streamThumbnails[stream.ownerUsername];
                  return (
                    <button
                      key={stream.streamId}
                      onClick={() => handleWatchStream(stream.ownerUsername)}
                      className={`group relative overflow-hidden rounded-xl border transition-all hover:shadow-lg hover:shadow-accent/5 ${
                        isWatching
                          ? "border-accent/60 ring-1 ring-accent/30"
                          : "border-border bg-bg-primary hover:border-accent/50"
                      }`}
                    >
                      {/* Stream preview: live video if watching, thumbnail otherwise */}
                      <div className="relative aspect-video w-full bg-bg-secondary">
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
                              className="flex h-14 w-14 items-center justify-center rounded-2xl text-2xl font-bold text-white"
                              style={{ background: stringToGradient(stream.ownerUsername) }}
                            >
                              {stream.ownerUsername.charAt(0).toUpperCase()}
                            </div>
                          </div>
                        )}
                        {/* Live / Watching badge */}
                        <div className={`absolute left-2 top-2 flex items-center gap-1 rounded px-1.5 py-0.5 ${
                          isWatching ? "bg-accent/90" : "bg-error/90"
                        }`}>
                          <div className="h-1.5 w-1.5 rounded-full bg-white animate-pulse" />
                          <span className="text-[10px] font-bold text-white">
                            {isWatching ? "WATCHING" : "LIVE"}
                          </span>
                        </div>
                        {/* Hover overlay */}
                        <div className="absolute inset-0 flex items-center justify-center bg-black/0 transition-colors group-hover:bg-black/40">
                          <span className="text-sm font-semibold text-white opacity-0 transition-opacity group-hover:opacity-100">
                            {isWatching ? "Expand" : "Watch Stream"}
                          </span>
                        </div>
                      </div>
                      {/* Stream info */}
                      <div className="flex items-center gap-2.5 px-3 py-2.5">
                        <div
                          className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full text-[11px] font-bold text-white"
                          style={{ background: stringToGradient(stream.ownerUsername) }}
                        >
                          {stream.ownerUsername.charAt(0).toUpperCase()}
                        </div>
                        <div className="min-w-0 flex-1 text-left">
                          <div className="flex items-center gap-1.5">
                            <span className="truncate text-sm font-semibold text-text-bright">
                              {stream.ownerUsername}
                            </span>
                            {stream.hasAudio && (
                              <svg className="h-3.5 w-3.5 shrink-0 text-[#00bfff]" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
                                <path d="M11 5L6 9H2v6h4l5 4V5z" />
                                <path d="M15.54 8.46a5 5 0 010 7.07" />
                              </svg>
                            )}
                          </div>
                          <div className="text-[10px] text-text-muted">
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
                            className="ml-auto flex h-7 items-center gap-1.5 rounded-md bg-error/10 px-2.5 text-[11px] font-semibold text-error transition-colors hover:bg-error/20"
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
                className="flex cursor-pointer flex-col items-center rounded-2xl px-4 py-3 transition-all duration-200 hover:bg-surface-hover hover:shadow-[0_0_12px_rgba(255,255,255,0.04)]"
                onClick={(e) => {
                  const rect = e.currentTarget.getBoundingClientRect();
                  openProfilePopup(p.username, { x: rect.right + 8, y: rect.top });
                }}
                onContextMenu={(e) => {
                  e.preventDefault();
                  openContextMenu(p.username, { x: e.clientX, y: e.clientY });
                }}
              >
                <div className="relative mb-2">
                  <div
                    className={`flex h-20 w-20 items-center justify-center rounded-xl text-[28px] font-bold text-white transition-all duration-200 ${
                      isSpeaking ? "ring-[3px] ring-success" : ""
                    }`}
                    style={{ background: stringToGradient(p.username) }}
                  >
                    {p.username.charAt(0).toUpperCase()}
                  </div>
                  {p.isMuted && (
                    <div className="absolute -bottom-1 -right-1 flex h-[22px] w-[22px] items-center justify-center rounded-full border-2 border-bg-tertiary bg-error">
                      <svg className="h-3 w-3 text-white" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                        <line x1="1" y1="1" x2="23" y2="23" />
                        <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
                        <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
                        <line x1="12" y1="19" x2="12" y2="23" />
                        <line x1="8" y1="23" x2="16" y2="23" />
                      </svg>
                    </div>
                  )}
                </div>
                <div className="max-w-full truncate text-center text-xs font-semibold text-text-primary">
                  {p.username}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Bottom controls - hidden when stream is in real fullscreen */}
      {!isStreamFullscreen && (
        <div className="flex justify-center gap-3 border-t border-border bg-bg-primary px-5 py-3">
          <button
            onClick={handleMute}
            className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs font-semibold transition-colors ${
              isMuted
                ? "bg-error/20 text-error"
                : "bg-surface-active text-text-secondary hover:bg-border hover:text-text-primary"
            }`}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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
            className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs font-semibold transition-colors ${
              isDeafened
                ? "bg-error/20 text-error"
                : "bg-surface-active text-text-secondary hover:bg-border hover:text-text-primary"
            }`}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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
            className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs font-semibold transition-colors ${
              isStreaming
                ? "bg-accent/20 text-accent hover:bg-accent/30"
                : "bg-surface-active text-text-secondary hover:bg-border hover:text-text-primary"
            }`}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
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
            className="flex items-center gap-1.5 rounded-lg bg-error px-5 py-2 text-xs font-semibold text-white transition-colors hover:bg-error/80"
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
