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
                {!isOwnStream && stream?.hasAudio && (
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
                ? `overflow-hidden ${overlayVisible ? "cursor-default" : "cursor-none"}`
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

            {/* Fullscreen bottom control bar — slides up/down */}
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
                  <div className="ml-2 flex -space-x-1.5">
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
                </div>

                {/* Control bar */}
                <div className="mb-4 flex items-center gap-2 rounded-xl border border-white/10 bg-[#1e1f22]/95 px-3 py-2 shadow-2xl backdrop-blur-sm">
                  {/* Mute */}
                  <button
                    onClick={handleMute}
                    className={`flex h-9 w-9 items-center justify-center rounded-lg transition-colors ${
                      isMuted
                        ? "bg-white/15 text-error hover:bg-white/20"
                        : "text-white/80 hover:bg-white/10 hover:text-white"
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
                        : "text-white/80 hover:bg-white/10 hover:text-white"
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

                  {/* Divider */}
                  <div className="mx-1 h-6 w-px bg-white/15" />

                  {/* Stream volume */}
                  {!isOwnStream && stream?.hasAudio && (
                    <>
                      <button
                        onClick={toggleMute}
                        className={`flex h-9 w-9 items-center justify-center rounded-lg transition-colors ${
                          streamVolume === 0
                            ? "bg-white/15 text-error hover:bg-white/20"
                            : "text-white/80 hover:bg-white/10 hover:text-white"
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
                        className="h-1 w-20 cursor-pointer appearance-none rounded-full bg-white/20 accent-accent [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:bg-accent"
                        title={`Stream volume: ${streamVolume}%`}
                      />
                      <div className="mx-1 h-6 w-px bg-white/15" />
                    </>
                  )}

                  {/* Stop watching */}
                  <button
                    onClick={handleStopWatching}
                    className="flex h-9 items-center gap-1.5 rounded-lg bg-error/80 px-3 text-[12px] font-semibold text-white transition-colors hover:bg-error"
                    title="Stop watching"
                  >
                    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                      <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
                      <rect x="8" y="8" width="8" height="8" rx="1" fill="currentColor" stroke="none" />
                    </svg>
                    Stop Watching
                  </button>

                  {/* Exit fullscreen */}
                  <button
                    onClick={(e) => { e.stopPropagation(); exitFullscreen(); }}
                    className="flex h-9 w-9 items-center justify-center rounded-lg text-white/80 transition-colors hover:bg-white/10 hover:text-white"
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
