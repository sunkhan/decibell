import { useState, useRef, useCallback, useEffect } from "react";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import StreamVideoPlayer from "./StreamVideoPlayer";

function VolumeIcon({ muted }: { muted: boolean }) {
  if (muted) {
    return (
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M11 5L6 9H2v6h4l5 4V5z" />
        <line x1="23" y1="9" x2="17" y2="15" />
        <line x1="17" y1="9" x2="23" y2="15" />
      </svg>
    );
  }
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M11 5L6 9H2v6h4l5 4V5z" />
      <path d="M19.07 4.93a10 10 0 010 14.14" />
      <path d="M15.54 8.46a5 5 0 010 7.07" />
    </svg>
  );
}

export default function StreamViewPanel() {
  const fullscreenStream = useVoiceStore((s) => s.fullscreenStream);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const participants = useVoiceStore((s) => s.participants);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const watchingStreams = useVoiceStore((s) => s.watchingStreams);
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);

  const currentUsername = useAuthStore((s) => s.username);
  const isFullscreen = useVoiceStore((s) => s.isStreamFullscreen);
  const setIsFullscreen = useVoiceStore((s) => s.setStreamFullscreen);
  const [overlayVisible, setOverlayVisible] = useState(false);
  const overlayTimeout = useRef<ReturnType<typeof setTimeout>>();
  const [streamVolume, setStreamVolume] = useState(100);
  const prevVolume = useRef(100);

  const [lastStreamUser, setLastStreamUser] = useState<string | null>(null);
  useEffect(() => {
    if (fullscreenStream) setLastStreamUser(fullscreenStream);
  }, [fullscreenStream]);

  const displayUser = fullscreenStream || lastStreamUser;

  const appWindow = getCurrentWindow();

  const enterFullscreen = useCallback(async () => {
    setIsFullscreen(true);
    await appWindow.setFullscreen(true).catch(() => {});
  }, [appWindow, setIsFullscreen]);

  const exitFullscreen = useCallback(async () => {
    setIsFullscreen(false);
    setOverlayVisible(false);
    await appWindow.setFullscreen(false).catch(() => {});
  }, [appWindow, setIsFullscreen]);

  useEffect(() => {
    if (!isFullscreen) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") exitFullscreen();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [isFullscreen, exitFullscreen]);

  const handleVolumeChange = (value: number) => {
    setStreamVolume(value);
    invoke("set_stream_volume", { volume: value / 100 }).catch(console.error);
  };

  const toggleMute = () => {
    if (streamVolume > 0) {
      prevVolume.current = streamVolume;
      handleVolumeChange(0);
    } else {
      handleVolumeChange(prevVolume.current || 100);
    }
  };

  const stream = activeStreams.find((s) => s.ownerUsername === displayUser);
  const isOwnStream = displayUser === currentUsername;

  // Mute stream audio when watching own stream to prevent echo
  useEffect(() => {
    if (isOwnStream) {
      invoke("set_stream_volume", { volume: 0 }).catch(() => {});
    }
  }, [isOwnStream]);

  const handleMouseMove = useCallback(() => {
    if (!isFullscreen) return;
    setOverlayVisible(true);
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    overlayTimeout.current = setTimeout(() => setOverlayVisible(false), 3000);
  }, [isFullscreen]);

  const handleMouseLeave = useCallback(() => {
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    setOverlayVisible(false);
  }, []);

  const handleBackToCards = () => {
    if (isFullscreen) exitFullscreen();
    useVoiceStore.getState().setFullscreenStream(null);
  };

  const handleStopWatching = async () => {
    if (!displayUser || !connectedServerId || !connectedChannelId) return;
    await invoke("stop_watching", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
      targetUsername: displayUser,
    }).catch(() => {});
    useVoiceStore.getState().removeWatching(displayUser);
    if (isFullscreen) exitFullscreen();
  };

  const handleSwitchStream = (username: string) => {
    useVoiceStore.getState().setFullscreenStream(username);
  };

  if (!displayUser || !stream) return null;

  const resLabel = stream.resolutionWidth > 0 ? `${stream.resolutionHeight}p` : "";
  const fpsLabel = stream.fps > 0 ? `${stream.fps}fps` : "";
  const qualityBadge = [resLabel, fpsLabel].filter(Boolean).join(" · ");

  // Single root div: switches between in-flow layout and fixed fullscreen overlay
  return (
    <div
      className={
        isFullscreen
          ? "fixed inset-0 z-50 flex flex-col bg-black"
          : "flex flex-1 flex-col bg-bg-primary"
      }
    >
      <div className="flex min-w-0 flex-1">
        <div className={`flex min-w-0 flex-1 flex-col ${isFullscreen ? "" : "p-2"}`}>
          {/* Header bar — hidden in fullscreen */}
          {!isFullscreen && (
            <div className="mb-1.5 flex items-center gap-2">
              <div
                className="flex h-5 w-5 items-center justify-center rounded-md text-[9px] font-bold text-white"
                style={{ background: stringToGradient(displayUser) }}
              >
                {displayUser.charAt(0).toUpperCase()}
              </div>
              <span className="text-xs font-bold text-text-bright">
                {displayUser}'s screen
              </span>
              {qualityBadge && (
                <span className="text-[10px] text-text-muted">{qualityBadge}</span>
              )}
              <div className="ml-auto flex items-center gap-2">
                {!isOwnStream && (
                  <div className="flex items-center gap-1.5">
                    <button
                      onClick={toggleMute}
                      className="flex h-6 w-6 items-center justify-center rounded-md text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
                      title={streamVolume > 0 ? "Mute stream" : "Unmute stream"}
                    >
                      <VolumeIcon muted={streamVolume === 0} />
                    </button>
                    <input
                      type="range"
                      min={0}
                      max={100}
                      value={streamVolume}
                      onChange={(e) => handleVolumeChange(Number(e.target.value))}
                      className="h-1 w-20 cursor-pointer appearance-none rounded-full bg-text-muted/20 accent-accent [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-accent"
                      title={`Stream volume: ${streamVolume}%`}
                    />
                  </div>
                )}
                <button
                  onClick={handleBackToCards}
                  className="rounded-md px-2 py-1 text-[10px] font-semibold text-text-secondary transition-colors hover:bg-surface-hover"
                >
                  Back
                </button>
                <button
                  onClick={handleStopWatching}
                  className="rounded-md px-2 py-1 text-[10px] font-semibold text-error transition-colors hover:bg-error/10"
                >
                  Stop
                </button>
                <button
                  onClick={enterFullscreen}
                  className="flex h-6 w-6 items-center justify-center rounded-md bg-surface-hover text-xs transition-colors hover:bg-border"
                  title="Fullscreen"
                >
                  &#x26F6;
                </button>
              </div>
            </div>
          )}

          {/* Stream video — THE single player instance, never unmounts */}
          <div
            className={`relative flex flex-1 items-center justify-center ${
              isFullscreen
                ? "cursor-default"
                : "cursor-pointer rounded-lg border border-border bg-bg-tertiary"
            }`}
            onClick={handleBackToCards}
            onMouseMove={isFullscreen ? handleMouseMove : undefined}
            onMouseLeave={isFullscreen ? handleMouseLeave : undefined}
          >
            <StreamVideoPlayer
              streamerUsername={displayUser}
              className={`h-full w-full object-contain ${isFullscreen ? "" : "rounded-lg"}`}
            />

            {/* Fullscreen hover overlay controls */}
            {isFullscreen && (
              <div
                className={`absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/90 to-transparent px-4 pb-3 pt-8 transition-opacity duration-300 ${
                  overlayVisible ? "opacity-100" : "pointer-events-none opacity-0"
                }`}
                onClick={(e) => e.stopPropagation()}
                onMouseEnter={() => {
                  if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
                  setOverlayVisible(true);
                }}
              >
                <div className="flex items-center justify-between">
                  <div className="flex items-center gap-2">
                    <div
                      className="flex h-[22px] w-[22px] items-center justify-center rounded-md text-[10px] font-bold text-white"
                      style={{ background: stringToGradient(displayUser) }}
                    >
                      {displayUser.charAt(0).toUpperCase()}
                    </div>
                    <span className="text-xs font-semibold text-white">
                      {displayUser}'s screen
                    </span>
                    {qualityBadge && (
                      <span className="text-[10px] text-white/60">{qualityBadge}</span>
                    )}
                  </div>
                  <div className="flex items-center gap-3">
                    {!isOwnStream && (
                      <div className="flex items-center gap-1.5">
                        <button
                          onClick={(e) => { e.stopPropagation(); toggleMute(); }}
                          className="flex h-7 w-7 items-center justify-center rounded-md border border-white/20 bg-bg-primary/50 text-white/80 transition-colors hover:bg-bg-primary hover:text-white"
                          title={streamVolume > 0 ? "Mute stream" : "Unmute stream"}
                        >
                          <VolumeIcon muted={streamVolume === 0} />
                        </button>
                        <input
                          type="range"
                          min={0}
                          max={100}
                          value={streamVolume}
                          onChange={(e) => handleVolumeChange(Number(e.target.value))}
                          onClick={(e) => e.stopPropagation()}
                          className="h-1 w-24 cursor-pointer appearance-none rounded-full bg-white/20 accent-accent [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-accent"
                          title={`Stream volume: ${streamVolume}%`}
                        />
                      </div>
                    )}
                    <div className="flex -space-x-1.5">
                      {participants.slice(0, 4).map((p) => (
                        <div
                          key={p.username}
                          className="flex h-6 w-6 items-center justify-center rounded-md border-2 border-black text-[9px] font-bold text-white"
                          style={{ background: stringToGradient(p.username) }}
                        >
                          {p.username.charAt(0).toUpperCase()}
                        </div>
                      ))}
                    </div>
                    <button
                      onClick={(e) => { e.stopPropagation(); exitFullscreen(); }}
                      className="flex h-7 w-7 items-center justify-center rounded-md border border-white/20 bg-bg-primary/50 text-sm transition-colors hover:bg-bg-primary"
                      title="Exit fullscreen"
                    >
                      &#x26F6;
                    </button>
                  </div>
                </div>
              </div>
            )}
          </div>
        </div>

        {/* Sidebar — hidden in fullscreen */}
        {!isFullscreen && (
          <div className="flex w-[140px] shrink-0 flex-col gap-1 border-l border-border p-2">
            <h4 className="px-1 text-[9px] font-bold uppercase tracking-wider text-text-muted">
              Voice — {participants.length}
            </h4>

            {participants.map((p) => {
              const isStreaming = activeStreams.some((s) => s.ownerUsername === p.username);
              const isSpeaking = speakingUsers.includes(p.username);
              return (
                <div
                  key={p.username}
                  className="flex cursor-pointer items-center gap-1.5 rounded-md p-1.5 transition-colors hover:bg-surface-hover"
                  onClick={(e) => {
                    const rect = e.currentTarget.getBoundingClientRect();
                    openProfilePopup(p.username, { x: rect.right + 8, y: rect.top });
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    openContextMenu(p.username, { x: e.clientX, y: e.clientY });
                  }}
                >
                  <div
                    className={`flex h-7 w-7 items-center justify-center rounded-md text-[11px] font-bold text-white ${
                      isSpeaking ? "ring-2 ring-success ring-offset-1 ring-offset-bg-primary" : ""
                    }`}
                    style={{ background: stringToGradient(p.username) }}
                  >
                    {p.username.charAt(0).toUpperCase()}
                  </div>
                  <div className="min-w-0">
                    <div className="truncate text-[11px] font-semibold text-text-secondary">
                      {p.username}
                    </div>
                    {isStreaming && (
                      <div className="text-[9px] text-error">Streaming</div>
                    )}
                  </div>
                </div>
              );
            })}

            {activeStreams.length > 1 && (
              <div className="mt-auto border-t border-border pt-2">
                <h4 className="mb-1 px-1 text-[9px] font-bold uppercase tracking-wider text-text-muted">
                  Streams
                </h4>
                {activeStreams.filter((s) => watchingStreams.includes(s.ownerUsername)).map((s) => (
                  <button
                    key={s.ownerUsername}
                    onClick={() => handleSwitchStream(s.ownerUsername)}
                    className={`w-full rounded-md px-2 py-1.5 text-left text-[10px] font-semibold transition-colors ${
                      s.ownerUsername === displayUser
                        ? "border-l-2 border-accent bg-accent/10 text-accent"
                        : "text-text-secondary hover:bg-surface-hover"
                    }`}
                  >
                    {s.ownerUsername}'s screen
                  </button>
                ))}
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
