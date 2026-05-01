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
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
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
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (isFullscreen) {
          exitFullscreen();
        } else {
          useVoiceStore.getState().setFullscreenStream(null);
        }
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [isFullscreen, exitFullscreen]);

  useEffect(() => {
    if (!isFullscreen || !displayUser) return;
    const streamStillActive = activeStreams.some((s) => s.ownerUsername === displayUser);
    if (!streamStillActive) {
      exitFullscreen();
    }
  }, [isFullscreen, displayUser, activeStreams, exitFullscreen]);

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

  const stream = activeStreams.find((s) => s.ownerUsername === displayUser);
  const isOwnStream = displayUser === currentUsername;

  useEffect(() => {
    if (isOwnStream) {
      invoke("set_stream_volume", { volume: 0 }).catch(() => {});
    }
  }, [isOwnStream]);

  const [hoverControlsVisible, setHoverControlsVisible] = useState(false);
  const hoverTimeout = useRef<ReturnType<typeof setTimeout>>();

  const handleMouseMove = useCallback(() => {
    if (isFullscreen) {
      setOverlayVisible(true);
      if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
      overlayTimeout.current = setTimeout(() => setOverlayVisible(false), 3000);
    } else {
      setHoverControlsVisible(true);
      if (hoverTimeout.current) clearTimeout(hoverTimeout.current);
      hoverTimeout.current = setTimeout(() => setHoverControlsVisible(false), 1500);
    }
  }, [isFullscreen]);

  const handleMouseLeave = useCallback(() => {
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    setOverlayVisible(false);
    if (hoverTimeout.current) clearTimeout(hoverTimeout.current);
    setHoverControlsVisible(false);
  }, []);

  const handleBackToCards = () => {
    if (isFullscreen) exitFullscreen();
    useVoiceStore.getState().setFullscreenStream(null);
  };

  const handleStopWatching = async () => {
    if (!displayUser || !connectedServerId || !connectedChannelId) return;
    if (displayUser === currentUsername) {
      await invoke("watch_self_stream", { enabled: false }).catch(() => {});
    } else {
      await invoke("stop_watching", {
        serverId: connectedServerId,
        channelId: connectedChannelId,
        targetUsername: displayUser,
      }).catch(() => {});
    }
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

  return (
    <div
      className={
        isFullscreen
          ? "fixed inset-0 z-50 flex flex-col bg-black"
          : "flex flex-1 flex-col bg-bg-dark"
      }
    >
      <div className="flex min-h-0 min-w-0 flex-1">
        <div className={`flex min-h-0 min-w-0 flex-1 flex-col ${isFullscreen ? "" : "p-2"}`}>
          {/* Header bar */}
          {!isFullscreen && (
            <div className="mb-2 flex items-center gap-2.5 px-1">
              <div
                className="flex h-6 w-6 items-center justify-center rounded-md text-[10px] font-semibold text-white"
                style={{ background: stringToGradient(displayUser) }}
              >
                {displayUser.charAt(0).toUpperCase()}
              </div>
              <span className="text-[13px] font-medium text-text-primary">
                {displayUser}'s screen
              </span>
              {qualityBadge && (
                <span className="text-[11px] text-text-muted">{qualityBadge}</span>
              )}
              <div className="ml-auto flex items-center gap-2">
                {!isOwnStream && stream?.hasAudio && (
                  <div className="flex items-center gap-1.5">
                    <button
                      onClick={toggleMute}
                      className={`flex h-6 w-6 items-center justify-center rounded-md transition-colors ${
                        streamVolume === 0 ? "text-error" : "text-text-muted hover:bg-surface-hover hover:text-text-secondary"
                      }`}
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
                      className="h-[4px] w-20 cursor-pointer appearance-none rounded-full bg-bg-lighter accent-accent [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-dark [&::-webkit-slider-thumb]:shadow-[0_0_6px_rgba(56,143,255,0.3)]"
                      title={`Stream volume: ${streamVolume}%`}
                    />
                  </div>
                )}
                <button
                  onClick={handleBackToCards}
                  className="flex h-6 w-6 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
                  title="Back (Esc)"
                >
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
                    <line x1="19" y1="12" x2="5" y2="12" />
                    <polyline points="12 19 5 12 12 5" />
                  </svg>
                </button>
              </div>
            </div>
          )}

          {/* Stream video */}
          <div
            className={`relative flex min-h-0 flex-1 items-center justify-center overflow-hidden ${
              isFullscreen
                ? `${overlayVisible ? "cursor-default" : "cursor-none"}`
                : "cursor-pointer rounded-xl border border-border bg-bg-darkest"
            }`}
            onClick={handleBackToCards}
            onMouseMove={handleMouseMove}
            onMouseLeave={handleMouseLeave}
          >
            <StreamVideoPlayer
              streamerUsername={displayUser}
              className={`h-full w-full object-contain ${isFullscreen ? "" : "rounded-xl"}`}
            />

            {/* Non-fullscreen hover controls */}
            {!isFullscreen && (
              <div
                className={`absolute bottom-3 right-3 flex items-center gap-2 rounded-[10px] border border-border bg-bg-light/95 px-2.5 py-1.5 shadow-[0_8px_32px_rgba(0,0,0,0.4)] transition-opacity duration-200 ${
                  hoverControlsVisible ? "opacity-100" : "pointer-events-none opacity-0"
                }`}
                onClick={(e) => e.stopPropagation()}
                onMouseEnter={() => {
                  if (hoverTimeout.current) clearTimeout(hoverTimeout.current);
                  setHoverControlsVisible(true);
                }}
              >
                {!isOwnStream && stream?.hasAudio && (
                  <>
                    <button
                      onClick={toggleMute}
                      className={`flex h-7 w-7 items-center justify-center rounded-md transition-colors ${
                        streamVolume === 0
                          ? "text-error hover:bg-white/[0.08]"
                          : "text-white/80 hover:bg-white/[0.08] hover:text-white"
                      }`}
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
                      className="h-[4px] w-16 cursor-pointer appearance-none rounded-full bg-white/15 accent-accent [&::-webkit-slider-thumb]:h-2.5 [&::-webkit-slider-thumb]:w-2.5 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-darkest [&::-webkit-slider-thumb]:shadow-[0_0_6px_rgba(56,143,255,0.3)]"
                      title={`Stream volume: ${streamVolume}%`}
                    />
                    <div className="mx-0.5 h-5 w-px bg-white/10" />
                  </>
                )}
                <button
                  onClick={enterFullscreen}
                  className="flex h-7 w-7 items-center justify-center rounded-md text-white/80 transition-colors hover:bg-white/[0.08] hover:text-white"
                  title="Fullscreen"
                >
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3" />
                  </svg>
                </button>
              </div>
            )}

            {/* Fullscreen bottom control bar */}
            {isFullscreen && (
              <div
                className={`absolute inset-x-0 bottom-0 flex flex-col items-center transition-transform duration-300 ease-in-out ${
                  overlayVisible ? "translate-y-0" : "translate-y-full"
                }`}
                onClick={(e) => e.stopPropagation()}
                onMouseEnter={() => {
                  if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
                  setOverlayVisible(true);
                }}
              >
                {/* Info row */}
                <div className="mb-2 flex items-center gap-2">
                  <div
                    className="flex h-[22px] w-[22px] items-center justify-center rounded-md text-[10px] font-semibold text-white"
                    style={{ background: stringToGradient(displayUser) }}
                  >
                    {displayUser.charAt(0).toUpperCase()}
                  </div>
                  <span className="text-[12px] font-medium text-white">
                    {displayUser}'s screen
                  </span>
                  {qualityBadge && (
                    <span className="text-[10px] text-white/60">{qualityBadge}</span>
                  )}
                  <div className="ml-2 flex -space-x-1.5">
                    {participants.slice(0, 4).map((p) => (
                      <div
                        key={p.username}
                        className="flex h-6 w-6 items-center justify-center rounded-md border-2 border-black text-[9px] font-semibold text-white"
                        style={{ background: stringToGradient(p.username) }}
                      >
                        {p.username.charAt(0).toUpperCase()}
                      </div>
                    ))}
                  </div>
                </div>

                {/* Control bar */}
                <div className="mb-4 flex items-center gap-2 rounded-xl border border-border bg-bg-light/95 px-3 py-2 shadow-[0_8px_32px_rgba(0,0,0,0.5)]">
                  {/* Mute */}
                  <button
                    onClick={handleMute}
                    className={`flex h-9 w-9 items-center justify-center rounded-lg transition-colors ${
                      isMuted
                        ? "bg-white/15 text-error hover:bg-white/20"
                        : "text-white/80 hover:bg-white/[0.08] hover:text-white"
                    }`}
                    title={isMuted ? "Unmute" : "Mute"}
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
                    className={`flex h-9 w-9 items-center justify-center rounded-lg transition-colors ${
                      isDeafened
                        ? "bg-white/15 text-error hover:bg-white/20"
                        : "text-white/80 hover:bg-white/[0.08] hover:text-white"
                    }`}
                    title={isDeafened ? "Undeafen" : "Deafen"}
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

                  <div className="mx-1 h-6 w-px bg-white/10" />

                  {/* Stream volume */}
                  {!isOwnStream && stream?.hasAudio && (
                    <>
                      <button
                        onClick={toggleMute}
                        className={`flex h-9 w-9 items-center justify-center rounded-lg transition-colors ${
                          streamVolume === 0
                            ? "bg-white/15 text-error hover:bg-white/20"
                            : "text-white/80 hover:bg-white/[0.08] hover:text-white"
                        }`}
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
                        className="h-[4px] w-20 cursor-pointer appearance-none rounded-full bg-white/15 accent-accent [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-darkest [&::-webkit-slider-thumb]:shadow-[0_0_6px_rgba(56,143,255,0.3)]"
                        title={`Stream volume: ${streamVolume}%`}
                      />
                      <div className="mx-1 h-6 w-px bg-white/10" />
                    </>
                  )}

                  {/* Stop watching */}
                  <button
                    onClick={handleStopWatching}
                    className="flex h-9 items-center gap-1.5 rounded-lg bg-error/[0.15] px-3 text-[12px] font-medium text-error transition-colors hover:bg-error/[0.25]"
                    title="Stop watching"
                  >
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <rect x="2" y="3" width="20" height="14" rx="2" />
                      <line x1="8" y1="21" x2="16" y2="21" />
                      <line x1="12" y1="17" x2="12" y2="21" />
                      <line x1="7" y1="7" x2="17" y2="13" />
                      <line x1="17" y1="7" x2="7" y2="13" />
                    </svg>
                    Stop Watching
                  </button>

                  {/* Exit fullscreen */}
                  <button
                    onClick={(e) => { e.stopPropagation(); exitFullscreen(); }}
                    className="flex h-9 w-9 items-center justify-center rounded-lg text-white/80 transition-colors hover:bg-white/[0.08] hover:text-white"
                    title="Exit fullscreen"
                  >
                    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                      <path d="M8 3v3a2 2 0 0 1-2 2H3m18 0h-3a2 2 0 0 1-2-2V3m0 18v-3a2 2 0 0 1 2-2h3M3 16h3a2 2 0 0 1 2 2v3" />
                    </svg>
                  </button>
                </div>
              </div>
            )}
          </div>
        </div>

        {/* Sidebar */}
        {!isFullscreen && (
          <div className="flex w-[160px] shrink-0 flex-col gap-1 border-l border-border-divider bg-bg-dark p-3">
            <h4 className="mb-1 px-1 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
              Voice — {participants.length}
            </h4>

            {participants.map((p) => {
              const isStreaming = activeStreams.some((s) => s.ownerUsername === p.username);
              const isSpeaking = speakingUsers.includes(p.username);
              return (
                <div
                  key={p.username}
                  className="flex cursor-pointer items-center gap-2 rounded-[10px] p-2 transition-colors hover:bg-surface-hover"
                  onClick={(e) => {
                    const rect = e.currentTarget.getBoundingClientRect();
                    openProfilePopup(p.username, { x: rect.right + 8, y: rect.top }, connectedServerId);
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    openContextMenu(p.username, { x: e.clientX, y: e.clientY });
                  }}
                >
                  <div
                    className={`flex h-7 w-7 items-center justify-center rounded-md text-[11px] font-semibold text-white ${
                      isSpeaking ? "shadow-[0_0_0_2px_var(--color-bg-dark),0_0_0_3.5px_var(--color-success)]" : ""
                    }`}
                    style={{ background: stringToGradient(p.username) }}
                  >
                    {p.username.charAt(0).toUpperCase()}
                  </div>
                  <div className="min-w-0">
                    <div className="truncate text-[11px] font-medium text-text-secondary">
                      {p.username}
                    </div>
                    {isStreaming && (
                      <div className="text-[9px] font-medium text-accent">Streaming</div>
                    )}
                  </div>
                </div>
              );
            })}

            {activeStreams.length > 1 && (
              <div className="mt-auto border-t border-border-divider pt-2">
                <h4 className="mb-1 px-1 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
                  Streams
                </h4>
                {activeStreams.filter((s) => watchingStreams.includes(s.ownerUsername)).map((s) => (
                  <button
                    key={s.ownerUsername}
                    onClick={() => handleSwitchStream(s.ownerUsername)}
                    className={`w-full rounded-md px-2 py-1.5 text-left text-[10px] font-medium transition-colors ${
                      s.ownerUsername === displayUser
                        ? "border-l-2 border-accent bg-accent-soft text-accent-bright"
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
