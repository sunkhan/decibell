import { useState, useRef, useCallback } from "react";
import { useVoiceStore } from "../../stores/voiceStore";
import { stringToGradient } from "../../utils/colors";
import { invoke } from "@tauri-apps/api/core";
import StreamVideoPlayer from "./StreamVideoPlayer";

export default function StreamViewPanel() {
  const watching = useVoiceStore((s) => s.watching);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const participants = useVoiceStore((s) => s.participants);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);

  const [theaterMode, setTheaterMode] = useState(false);
  const [overlayVisible, setOverlayVisible] = useState(false);
  const overlayTimeout = useRef<ReturnType<typeof setTimeout>>();

  const stream = activeStreams.find((s) => s.ownerUsername === watching);

  const handleMouseMove = useCallback(() => {
    if (!theaterMode) return;
    setOverlayVisible(true);
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    overlayTimeout.current = setTimeout(() => setOverlayVisible(false), 3000);
  }, [theaterMode]);

  const handleMouseLeave = useCallback(() => {
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    setOverlayVisible(false);
  }, []);

  const handleSwitchStream = async (username: string) => {
    if (!connectedServerId || !connectedChannelId) return;
    if (watching) {
      await invoke("stop_watching", {
        serverId: connectedServerId,
        channelId: connectedChannelId,
        targetUsername: watching,
      }).catch(() => {});
    }
    await invoke("watch_stream", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
      targetUsername: username,
    }).catch(() => {});
    useVoiceStore.getState().setWatching(username);
  };

  const handleStopWatching = async () => {
    if (!watching || !connectedServerId || !connectedChannelId) return;
    await invoke("stop_watching", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
      targetUsername: watching,
    }).catch(() => {});
    useVoiceStore.getState().setWatching(null);
    setTheaterMode(false);
  };

  if (!watching || !stream) return null;

  const resLabel = stream.resolutionWidth > 0
    ? `${stream.resolutionHeight}p`
    : "";
  const fpsLabel = stream.fps > 0 ? `${stream.fps}fps` : "";
  const qualityBadge = [resLabel, fpsLabel].filter(Boolean).join(" · ");

  if (theaterMode) {
    return (
      <div
        className="relative flex flex-1 cursor-none items-center justify-center bg-black"
        onMouseMove={handleMouseMove}
        onMouseLeave={handleMouseLeave}
      >
        <StreamVideoPlayer streamerUsername={watching} className="h-full w-full object-contain" />

        <div
          className={`absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/90 to-transparent px-4 pb-3 pt-8 transition-opacity duration-300 ${
            overlayVisible ? "opacity-100" : "pointer-events-none opacity-0"
          }`}
          onMouseEnter={() => {
            if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
            setOverlayVisible(true);
          }}
        >
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <div
                className="flex h-[22px] w-[22px] items-center justify-center rounded-md text-[10px] font-bold text-white"
                style={{ background: stringToGradient(watching) }}
              >
                {watching.charAt(0).toUpperCase()}
              </div>
              <span className="text-xs font-semibold text-white">
                {watching}'s screen
              </span>
              {qualityBadge && (
                <span className="text-[10px] text-white/60">{qualityBadge}</span>
              )}
            </div>
            <div className="flex items-center gap-2">
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
                onClick={() => setTheaterMode(false)}
                className="flex h-7 w-7 items-center justify-center rounded-md border border-white/20 bg-bg-primary/50 text-sm transition-colors hover:bg-bg-primary"
                title="Exit theater mode"
              >
                &#x26F6;
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-1 bg-bg-primary">
      <div className="flex min-w-0 flex-1 flex-col p-2">
        <div className="mb-1.5 flex items-center gap-2">
          <div
            className="flex h-5 w-5 items-center justify-center rounded-md text-[9px] font-bold text-white"
            style={{ background: stringToGradient(watching) }}
          >
            {watching.charAt(0).toUpperCase()}
          </div>
          <span className="text-xs font-bold text-text-bright">
            {watching}'s screen
          </span>
          {qualityBadge && (
            <span className="text-[10px] text-text-muted">{qualityBadge}</span>
          )}
          <div className="ml-auto flex gap-1">
            <button
              onClick={handleStopWatching}
              className="rounded-md px-2 py-1 text-[10px] font-semibold text-error transition-colors hover:bg-error/10"
            >
              Stop
            </button>
            <button
              onClick={() => setTheaterMode(true)}
              className="flex h-6 w-6 items-center justify-center rounded-md bg-surface-hover text-xs transition-colors hover:bg-border"
              title="Theater mode"
            >
              &#x26F6;
            </button>
          </div>
        </div>

        <div className="flex flex-1 items-center justify-center rounded-lg border border-border bg-bg-tertiary">
          <StreamVideoPlayer streamerUsername={watching} className="h-full w-full rounded-lg object-contain" />
        </div>
      </div>

      <div className="flex w-[140px] shrink-0 flex-col gap-1 border-l border-border p-2">
        <h4 className="px-1 text-[9px] font-bold uppercase tracking-wider text-text-muted">
          Voice — {participants.length}
        </h4>

        {participants.map((p) => {
          const isStreaming = activeStreams.some((s) => s.ownerUsername === p.username);
          const isSpeaking = speakingUsers.includes(p.username);
          return (
            <div key={p.username} className="flex items-center gap-1.5 rounded-md p-1.5">
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
            {activeStreams.map((s) => (
              <button
                key={s.ownerUsername}
                onClick={() => handleSwitchStream(s.ownerUsername)}
                className={`w-full rounded-md px-2 py-1.5 text-left text-[10px] font-semibold transition-colors ${
                  s.ownerUsername === watching
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
    </div>
  );
}
