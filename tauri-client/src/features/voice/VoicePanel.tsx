import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToGradient } from "../../utils/colors";
import StreamViewPanel from "./StreamViewPanel";
import StreamVideoPlayer from "./StreamVideoPlayer";
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
  const disconnect = useVoiceStore((s) => s.disconnect);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const channels = useChatStore((s) => {
    const serverId = s.activeServerId;
    return serverId ? s.channelsByServer[serverId] ?? EMPTY_CHANNELS : EMPTY_CHANNELS;
  });

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

  const handleWatchStream = async (username: string) => {
    if (!connectedServerId || !connectedChannelId) return;
    const isAlreadyWatching = watchingStreams.includes(username);
    if (!isAlreadyWatching) {
      // Tell server to start forwarding this stream's frames
      await invoke("watch_stream", {
        serverId: connectedServerId,
        channelId: connectedChannelId,
        targetUsername: username,
      }).catch(() => {});
      useVoiceStore.getState().addWatching(username);
    }
    // Go fullscreen for this stream
    useVoiceStore.getState().setFullscreenStream(username);
  };

  const handleDisconnect = async () => {
    // Stop watching all streams
    if (connectedServerId && connectedChannelId) {
      for (const username of watchingStreams) {
        await invoke("stop_watching", {
          serverId: connectedServerId,
          channelId: connectedChannelId,
          targetUsername: username,
        }).catch(() => {});
      }
    }
    invoke("leave_voice_channel").catch(console.error);
    disconnect();
    setActiveView("server");
  };

  return (
    <div className="flex flex-1 flex-col bg-bg-tertiary">
      {/* Header */}
      <div className="flex items-center gap-2.5 border-b border-border px-4 py-3">
        <span className="text-accent">🔊</span>
        <span className="text-sm font-bold text-text-bright">{channelName}</span>
        <span
          className="ml-auto text-xs text-text-muted"
          title={latencyMs != null ? `${latencyMs}ms` : undefined}
        >
          {participants.length} participant{participants.length !== 1 ? "s" : ""}
          {latencyMs != null && (
            <span className="ml-2 text-text-muted">{latencyMs}ms</span>
          )}
        </span>
      </div>

      {/* Main content: fullscreen stream, stream cards, or participant grid */}
      {fullscreenStream ? (
        <StreamViewPanel />
      ) : hasStreams ? (
        /* Two-column layout: Users left, Streams right */
        <div className="flex flex-1 overflow-hidden">
          {/* Left: Users */}
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
                      <span className="text-[10px] text-error">🔇</span>
                    )}
                  </div>
                );
              })}
            </div>
          </div>

          {/* Right: Stream cards */}
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
                          <div className="truncate text-sm font-semibold text-text-bright">
                            {stream.ownerUsername}
                          </div>
                          <div className="text-[10px] text-text-muted">
                            {stream.resolutionWidth > 0
                              ? `${stream.resolutionWidth}x${stream.resolutionHeight}`
                              : ""}
                            {stream.fps > 0 ? ` · ${stream.fps}fps` : ""}
                            {stream.hasAudio ? " · Audio" : ""}
                          </div>
                        </div>
                      </div>
                    </button>
                  );
                })}
              </div>
            </div>
          </div>
        </div>
      ) : (
        /* No streams: centered participant grid (original layout) */
        <div className="flex flex-1 flex-wrap items-center justify-center gap-5 p-6">
          {participants.map((p) => {
            const isSpeaking = speakingUsers.includes(p.username);
            return (
              <div key={p.username} className="w-[100px] text-center">
                <div className="relative mx-auto mb-2">
                  <div
                    className={`flex h-20 w-20 items-center justify-center rounded-xl text-[28px] font-bold text-white transition-all duration-200 ${
                      isSpeaking ? "ring-[3px] ring-success" : ""
                    }`}
                    style={{ background: stringToGradient(p.username) }}
                  >
                    {p.username.charAt(0).toUpperCase()}
                  </div>
                  {p.isMuted && (
                    <div className="absolute -bottom-1 -right-1 flex h-[22px] w-[22px] items-center justify-center rounded-full border-2 border-bg-tertiary bg-error text-[10px]">
                      🔇
                    </div>
                  )}
                </div>
                <div className="text-xs font-semibold text-text-primary">
                  {p.username}
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Bottom controls */}
      <div className="flex justify-center gap-3 border-t border-border bg-bg-primary px-5 py-3">
        <button
          onClick={handleMute}
          className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs font-semibold transition-colors ${
            isMuted
              ? "bg-error/20 text-error"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
        >
          {isMuted ? "🔇 Unmute" : "🎤 Mute"}
        </button>
        <button
          onClick={handleDeafen}
          className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs font-semibold transition-colors ${
            isDeafened
              ? "bg-error/20 text-error"
              : "bg-surface-hover text-text-muted hover:bg-surface-active"
          }`}
        >
          {isDeafened ? "🔇 Undeafen" : "🎧 Deafen"}
        </button>
        <button
          onClick={handleDisconnect}
          className="flex items-center gap-1.5 rounded-lg bg-error px-5 py-2 text-xs font-semibold text-white transition-colors hover:bg-error/80"
        >
          Disconnect
        </button>
      </div>
    </div>
  );
}
