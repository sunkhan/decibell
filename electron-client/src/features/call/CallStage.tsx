import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "../../lib/ipc";
import { getCurrentWindow } from "../../lib/window";
import { useAuthStore } from "../../stores/authStore";
import { useCallStore } from "../../stores/callStore";
import { useDmStore } from "../../stores/dmStore";
import {
  CALL_STAGE_MIN,
  CALL_STAGE_STREAM_DEFAULT,
  CALL_STAGE_VOICE_DEFAULT,
  useUiStore,
} from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import { UserAvatar } from "../../components/UserAvatar";
import { playSound } from "../../utils/sounds";
import { VideoCodec, type StreamInfo } from "../../types";
import CaptureSourcePicker from "../voice/CaptureSourcePicker";
import { CodecBadge } from "../voice/CodecBadge";
import StreamStatsOverlay from "../voice/StreamStatsOverlay";
import { getStreamPipHost, placeStreamPip, recordFullViewRect } from "../voice/streamPipHost";
import { saveSettings } from "../settings/saveSettings";
import {
  DEFAULT_DB,
  MAX_DB,
  MIN_DB,
  dbToPercent,
  formatDb,
  pushUserGain,
} from "../voice/userGain";
import {
  announceCallStreamStop,
  endCall,
  unwatchCallStream,
  watchCallStream,
} from "./callActions";

/// The P2P call "stage": the top of the DM conversation while a call with
/// `peer` is ringing / connecting / active (design: Direction A,
/// docs/superpowers/specs/2026-08-28-p2p-dm-calls-design.md + the DM Call
/// Stage canvas).
///
///   voice   → two tiles on a dark stage, status pill, one control dock
///   stream  → the focused stream takes the stage over; tiles shrink to
///             chips; LIVE + quality pills; stats / theater / fullscreen
///   theater → the stage fills the DM panel (DmChatPanel hides the chat)
///   full    → window fullscreen, black, auto-hiding overlays, Esc exits
///
/// The video is the ONE persistent StreamVideoPlayer that StreamPipManager
/// owns: this component only claims its host node into a slot (the same
/// reparenting StreamViewPanel and the mini player do), so focusing a
/// stream here, minimising it to the mini player and coming back never
/// re-decodes. Which stream is "focused" is `voiceStore.fullscreenStream`
/// (the peer's, or our own self-preview).
export default function CallStage({ peer }: { peer: string }) {
  const status = useCallStore((s) => s.status);
  const callPeer = useCallStore((s) => s.peer);
  if (status === "idle" || status === "incoming" || callPeer !== peer) return null;
  return <Stage peer={peer} />;
}

const OVERLAY_HIDE_MS = 2500;

function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

function codecLabel(codec: number): string | null {
  switch (codec) {
    case VideoCodec.H264_HW:
    case VideoCodec.H264_SW:
      return "H.264";
    case VideoCodec.H265:
      return "HEVC";
    case VideoCodec.AV1:
      return "AV1";
    default:
      return null;
  }
}

function qualityLabel(info: StreamInfo | undefined): string {
  if (!info) return "";
  return [
    info.resolutionHeight > 0 ? `${info.resolutionHeight}p` : null,
    info.fps > 0 ? `${info.fps} fps` : null,
    codecLabel(info.currentCodec),
  ]
    .filter(Boolean)
    .join(" · ");
}

function Stage({ peer }: { peer: string }) {
  const me = useAuthStore((s) => s.username) ?? "";
  const status = useCallStore((s) => s.status);
  const ringingAcked = useCallStore((s) => s.ringingAcked);
  const startedAt = useCallStore((s) => s.startedAt);
  const connectedPath = useCallStore((s) => s.connectedPath);
  const theater = useCallStore((s) => s.theater);
  const setTheater = useCallStore((s) => s.setTheater);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const error = useVoiceStore((s) => s.error);
  const isStreaming = useVoiceStore((s) => s.isStreaming);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const watchingStreams = useVoiceStore((s) => s.watchingStreams);
  const fullscreenStream = useVoiceStore((s) => s.fullscreenStream);
  const isStreamFullscreen = useVoiceStore((s) => s.isStreamFullscreen);
  const setStreamFullscreen = useVoiceStore((s) => s.setStreamFullscreen);
  const msgCount = useDmStore((s) => s.conversations[peer]?.messages.length ?? 0);
  const voiceHeight = useUiStore((s) => s.callStageVoiceHeight);
  const streamHeight = useUiStore((s) => s.callStageStreamHeight);
  const setCallStageHeight = useUiStore((s) => s.setCallStageHeight);

  const live = status === "active";
  const peerStream = activeStreams.find((st) => st.ownerUsername === peer);
  const ownStream = activeStreams.find((st) => st.ownerUsername === me);
  // The stream shown on the stage: the peer's, or our own preview.
  const focused =
    fullscreenStream != null &&
    (fullscreenStream === peer || fullscreenStream === me) &&
    watchingStreams.includes(fullscreenStream) &&
    activeStreams.some((st) => st.ownerUsername === fullscreenStream)
      ? fullscreenStream
      : null;
  const focusedInfo = focused ? activeStreams.find((st) => st.ownerUsername === focused) : undefined;
  const isFullscreen = isStreamFullscreen && focused != null;

  const [showPicker, setShowPicker] = useState(false);
  const [showStats, setShowStats] = useState(false);
  const [streamVolume, setStreamVolume] = useState(100);
  const [overlayVisible, setOverlayVisible] = useState(true);
  const overlayTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const slotRef = useRef<HTMLDivElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const [resizing, setResizing] = useState(false);

  // 1 s tick for the duration readout.
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!live) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [live]);

  // Claim the persistent player into our slot whenever a stream is focused
  // here (re-runs on focus change so we reclaim it from the mini player).
  // On the way out, detach the host from our slot explicitly: it was
  // appended by hand, not by React, so if it ever stayed behind inside a
  // container React later reused for the tiles it would sit in the flex
  // row as a stray full-width child and squeeze the avatars.
  useLayoutEffect(() => {
    const slot = slotRef.current;
    if (focused && slot) {
      placeStreamPip(slot);
      recordFullViewRect(slot);
    }
    return () => {
      const host = getStreamPipHost();
      if (slot && host.parentElement === slot) host.remove();
    };
  }, [focused, isFullscreen, theater]);

  // ── fullscreen ──
  const appWindow = getCurrentWindow();
  const enterFullscreen = useCallback(async () => {
    setStreamFullscreen(true);
    setOverlayVisible(true);
    await appWindow.setFullscreen(true).catch(() => {});
  }, [appWindow, setStreamFullscreen]);
  const exitFullscreen = useCallback(async () => {
    setStreamFullscreen(false);
    setOverlayVisible(true);
    await appWindow.setFullscreen(false).catch(() => {});
  }, [appWindow, setStreamFullscreen]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (isFullscreen) void exitFullscreen();
      else if (theater) setTheater(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [isFullscreen, exitFullscreen, theater, setTheater]);

  // The focused stream ended (or was unwatched) while fullscreen → come back.
  useEffect(() => {
    if (isStreamFullscreen && !focused) void exitFullscreen();
  }, [isStreamFullscreen, focused, exitFullscreen]);

  // Fullscreen overlays auto-hide after a still cursor.
  const pokeOverlay = useCallback(() => {
    setOverlayVisible(true);
    if (overlayTimer.current) clearTimeout(overlayTimer.current);
    if (isFullscreen) {
      overlayTimer.current = setTimeout(() => setOverlayVisible(false), OVERLAY_HIDE_MS);
    }
  }, [isFullscreen]);
  useEffect(() => {
    pokeOverlay();
    return () => {
      if (overlayTimer.current) clearTimeout(overlayTimer.current);
    };
  }, [isFullscreen, pokeOverlay]);

  // ── handlers ──
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
  const handleHangUp = () => {
    if (isFullscreen) void exitFullscreen();
    void endCall(status === "outgoing" ? "Cancelled" : "Call ended");
  };
  const handleStopSharing = async () => {
    playSound("stream_stop");
    const { stopActiveStream } = await import("../voice/streaming/StreamCapture");
    await stopActiveStream();
    invoke("stop_screen_share", {}).catch(console.error);
    useVoiceStore.getState().setIsStreaming(false);
    announceCallStreamStop();
  };
  const handleStreamVolume = (value: number) => {
    setStreamVolume(value);
    invoke("set_stream_volume", { volume: value / 100 }).catch(console.error);
  };
  const handleStopWatching = () => {
    if (!focused) return;
    if (isFullscreen) void exitFullscreen();
    void unwatchCallStream(focused);
  };
  const toggleTheater = () => setTheater(!theater, msgCount);

  const statusText =
    status === "outgoing"
      ? ringingAcked
        ? "Ringing…"
        : "Calling…"
      : status === "connecting"
        ? "Connecting…"
        : live && startedAt
          ? [
              formatDuration(now - startedAt),
              latencyMs != null ? `${latencyMs} ms` : null,
              connectedPath === "host" ? "LAN" : connectedPath === "srflx" ? "direct" : null,
            ]
              .filter(Boolean)
              .join(" · ")
          : "";

  // ── resize (compact stage only): drag the bottom edge ──
  // Imperative during the drag (no per-frame React state), committed to
  // uiStore on release so it persists per mode. The chat below keeps at
  // least CHAT_MIN px; double-click resets the mode's default.
  const CHAT_MIN = 180;
  const compact = !isFullscreen && !theater;
  const mode: "voice" | "stream" = focused ? "stream" : "voice";
  const stageHeight = mode === "stream" ? streamHeight : voiceHeight;
  const maxStageHeight = () => {
    const parent = rootRef.current?.parentElement;
    return parent ? Math.max(CALL_STAGE_MIN, parent.clientHeight - 48 - CHAT_MIN) : Infinity;
  };
  const onResizeStart = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!compact || !rootRef.current) return;
    e.preventDefault();
    const el = rootRef.current;
    const startY = e.clientY;
    const startH = el.getBoundingClientRect().height;
    const maxH = maxStageHeight();
    let last = startH;
    setResizing(true);
    const onMove = (ev: PointerEvent) => {
      last = Math.min(maxH, Math.max(CALL_STAGE_MIN, startH + (ev.clientY - startY)));
      el.style.height = `${Math.round(last)}px`;
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      setResizing(false);
      setCallStageHeight(mode, last);
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  };
  const onResizeReset = () =>
    setCallStageHeight(mode, mode === "stream" ? CALL_STAGE_STREAM_DEFAULT : CALL_STAGE_VOICE_DEFAULT);

  // ── root ──
  const rootClass = isFullscreen
    ? `fixed inset-0 z-50 flex flex-col bg-black ${overlayVisible ? "cursor-default" : "cursor-none"}`
    : theater
      ? "relative flex min-h-0 flex-1 flex-col border-b border-border bg-bg-darkest"
      : "relative flex shrink-0 flex-col border-b border-border bg-bg-darkest";
  const rootStyle = compact
    ? { height: Math.min(stageHeight, maxStageHeight()), userSelect: resizing ? ("none" as const) : undefined }
    : undefined;
  const overlayClass = `transition-opacity duration-300 ${overlayVisible ? "opacity-100" : "opacity-0"}`;

  return (
    <div ref={rootRef} className={rootClass} style={rootStyle} onMouseMove={pokeOverlay}>
      {/* ── centre: video or tiles ── */}
      {/* Distinct keys: the video and tiles containers are both a <div> in the
          same position, so without keys React would reuse one DOM node for the
          other and the hand-parented player host could survive the switch. */}
      {focused ? (
        <div key="video" className="relative flex min-h-0 flex-1 items-center justify-center overflow-hidden bg-black">
          <div ref={slotRef} className="h-full w-full" />
          {showStats && <StreamStatsOverlay username={focused} />}
          <div className={`absolute bottom-4 right-4 flex gap-1.5 ${overlayClass}`}>
            <PeerChip username={me} muted={isMuted} />
            <PeerChip username={peer} />
          </div>
        </div>
      ) : (
        <div key="tiles" className="flex min-h-0 flex-1 flex-col items-center justify-center gap-4 pb-14 pt-10">
          {/* Tiles row: the two people, then a card per live screen share —
              a share is presented like another participant, not a banner
              competing with the peer's volume row. */}
          <div className="flex items-start justify-center gap-8">
            <CallTile username={me} muted={isMuted} dim={!live} />
            <div className="flex flex-col items-center">
              <CallTile username={peer} muted={false} dim={!live} pulse={!live} />
              {live && <PeerAudioControls username={peer} />}
            </div>
            {live && peerStream && (
              <StreamCard info={peerStream} owner={peer} own={false} />
            )}
            {live && isStreaming && ownStream && (
              <StreamCard info={ownStream} owner={me} own />
            )}
          </div>
          {error && live && (
            <div className="rounded-sm bg-error/10 px-2 py-1 text-[11px] text-error">{error}</div>
          )}
        </div>
      )}

      {/* ── top-left: status / LIVE + quality ── */}
      <div className={`pointer-events-none absolute left-4 top-3.5 flex gap-1.5 ${overlayClass}`}>
        {focused ? (
          <>
            <Pill live>
              {focused === me ? "Your screen" : `${focused}'s screen`}
            </Pill>
            {(qualityLabel(focusedInfo) || statusText) && (
              <Pill>{[qualityLabel(focusedInfo), statusText].filter(Boolean).join(" · ")}</Pill>
            )}
          </>
        ) : (
          statusText && <Pill dot={live}>{statusText}</Pill>
        )}
      </div>

      {/* ── top-right: stats / theater / fullscreen / stop watching ── */}
      <div className={`absolute right-4 top-3 flex gap-1 ${overlayClass}`}>
        {isFullscreen && (
          <span className="mr-2 self-center text-[11px] text-white/45">Esc to exit</span>
        )}
        {focused && (
          <OverlayButton title="Stream stats" active={showStats} onClick={() => setShowStats((v) => !v)}>
            <StatsIcon />
          </OverlayButton>
        )}
        {!isFullscreen && live && (
          <OverlayButton title={theater ? "Exit theater" : "Theater"} active={theater} onClick={toggleTheater}>
            {theater ? <CollapseIcon /> : <TheaterIcon />}
          </OverlayButton>
        )}
        {focused && (
          <OverlayButton
            title={isFullscreen ? "Exit fullscreen" : "Fullscreen"}
            onClick={() => void (isFullscreen ? exitFullscreen() : enterFullscreen())}
          >
            {isFullscreen ? <CollapseIcon /> : <FullscreenIcon />}
          </OverlayButton>
        )}
        {focused && !isFullscreen && (
          <OverlayButton title="Stop watching" onClick={handleStopWatching}>
            <CloseIcon />
          </OverlayButton>
        )}
      </div>

      {/* ── bottom: control dock ── */}
      <div className={`pointer-events-none absolute inset-x-0 bottom-4 flex justify-center ${overlayClass}`}>
        <div className="pointer-events-auto flex items-center gap-1.5 rounded-lg border border-border bg-bg-light p-1.5 shadow-float">
          {live && (
            <>
              <DockButton title={isMuted ? "Unmute" : "Mute"} onClick={handleMute} tone={isMuted ? "danger" : "soft"}>
                {isMuted ? <MicOffIcon /> : <MicIcon />}
              </DockButton>
              <DockButton title={isDeafened ? "Undeafen" : "Deafen"} onClick={handleDeafen} tone={isDeafened ? "danger" : "soft"}>
                {isDeafened ? <DeafenOffIcon /> : <DeafenIcon />}
              </DockButton>
              <DockButton
                title={isStreaming ? "Stop sharing" : "Share your screen"}
                onClick={isStreaming ? () => void handleStopSharing() : () => setShowPicker(true)}
                tone={isStreaming ? "active" : "soft"}
              >
                {isStreaming ? <StopShareIcon /> : <ShareIcon />}
              </DockButton>
              {focused && focused !== me && (
                <>
                  <DockDivider />
                  <div className="flex items-center gap-2 px-1.5 text-text-secondary">
                    <button
                      onClick={() => handleStreamVolume(streamVolume > 0 ? 0 : 100)}
                      title={streamVolume > 0 ? "Mute stream" : "Unmute stream"}
                      className="flex h-6 w-6 items-center justify-center rounded-sm hover:text-text-bright"
                    >
                      <VolumeIcon muted={streamVolume === 0} />
                    </button>
                    <input
                      type="range"
                      min={0}
                      max={100}
                      value={streamVolume}
                      onChange={(e) => handleStreamVolume(Number(e.target.value))}
                      title="Stream volume"
                      className="h-[4px] w-20 cursor-pointer appearance-none rounded-full bg-bg-lighter accent-accent [&::-webkit-slider-thumb]:h-[12px] [&::-webkit-slider-thumb]:w-[12px] [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-light"
                    />
                    <span className="w-8 text-[10.5px] tabular-nums text-text-muted">{streamVolume}%</span>
                  </div>
                </>
              )}
              <DockDivider />
            </>
          )}
          <button
            onClick={handleHangUp}
            title={status === "outgoing" ? "Cancel" : "Hang up"}
            className="flex h-10 items-center gap-2 rounded-md bg-error px-4 text-[13px] font-semibold text-text-bright transition-opacity hover:opacity-90"
          >
            <HangUpIcon />
            {status === "outgoing" ? "Cancel" : "Hang up"}
          </button>
        </div>
      </div>

      {/* ── bottom edge: drag to resize (compact only) ── */}
      {compact && (
        <div
          onPointerDown={onResizeStart}
          onDoubleClick={onResizeReset}
          title="Drag to resize · double-click to reset"
          className="group absolute inset-x-0 -bottom-1 z-10 flex h-2.5 cursor-ns-resize items-center justify-center"
        >
          <div
            className={`h-1 w-12 rounded-full transition-colors ${
              resizing ? "bg-accent" : "bg-border group-hover:bg-accent/70"
            }`}
          />
        </div>
      )}

      {showPicker && <CaptureSourcePicker onClose={() => setShowPicker(false)} />}
    </div>
  );
}

// ── pieces ────────────────────────────────────────────────────────

function Pill({ children, live, dot }: { children: React.ReactNode; live?: boolean; dot?: boolean }) {
  return (
    <div className="flex h-6 items-center gap-1.5 whitespace-nowrap rounded-sm border border-white/10 bg-black/70 px-2.5 text-[11px] tabular-nums text-text-primary">
      {live && <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-error" />}
      {!live && dot && <span className="h-1.5 w-1.5 rounded-full bg-success" />}
      {children}
    </div>
  );
}

function OverlayButton({
  title,
  onClick,
  active,
  children,
}: {
  title: string;
  onClick: () => void;
  active?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      title={title}
      className={`flex h-8 w-8 items-center justify-center rounded-sm border border-white/10 transition-colors ${
        active ? "bg-accent-soft text-accent" : "bg-black/70 text-text-primary hover:bg-black/85 hover:text-text-bright"
      }`}
    >
      {children}
    </button>
  );
}

function DockButton({
  title,
  onClick,
  tone,
  children,
}: {
  title: string;
  onClick: () => void;
  tone: "soft" | "active" | "danger";
  children: React.ReactNode;
}) {
  const tones = {
    soft: "bg-surface-hover text-text-secondary hover:bg-surface-active hover:text-text-bright",
    active: "bg-accent-soft text-accent hover:bg-accent-mid",
    danger: "bg-error/15 text-error hover:bg-error/25",
  } as const;
  return (
    <button
      onClick={onClick}
      title={title}
      className={`flex h-10 w-10 items-center justify-center rounded-md transition-colors ${tones[tone]}`}
    >
      {children}
    </button>
  );
}

function DockDivider() {
  return <div className="mx-0.5 h-6 w-px bg-border-divider" />;
}

interface CallTileProps {
  username: string;
  muted: boolean;
  dim: boolean;
  pulse?: boolean;
}

const CallTile = memo(function CallTile({ username, muted, dim, pulse }: CallTileProps) {
  // Per-tile speaking subscription so the other tile's events are no-ops.
  const isSpeaking = useVoiceStore((s) => s.speakingUsers.has(username));
  const peerMuted = useVoiceStore(
    (s) => s.participants.find((p) => p.username === username)?.isMuted ?? false,
  );
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  const showMuted = muted || peerMuted;
  return (
    <div
      className={`flex cursor-pointer flex-col items-center gap-2.5 rounded-lg px-4 py-2 transition-all hover:bg-surface-hover ${
        dim ? "opacity-70" : ""
      }`}
      onClick={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        openProfilePopup(username, { x: rect.right + 8, y: rect.top }, null);
      }}
      onContextMenu={(e) => {
        // Same menu as a voice-channel tile: local mute + user volume.
        e.preventDefault();
        openContextMenu(username, { x: e.clientX, y: e.clientY }, null);
      }}
    >
      <div className="relative">
        {pulse && <div className="absolute inset-0 animate-ping rounded-lg bg-accent/20" />}
        <div
          className={`relative rounded-lg transition-all duration-150 ${
            isSpeaking
              ? "shadow-[0_0_0_3px_var(--color-bg-darkest),0_0_0_5px_var(--color-success)]"
              : ""
          }`}
        >
          <UserAvatar username={username} size={88} />
        </div>
        {showMuted && (
          <div className="absolute -bottom-1 -right-1 flex h-[22px] w-[22px] items-center justify-center rounded-full border-[2.5px] border-bg-darkest bg-bg-light">
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
      <div className="max-w-[10rem] truncate text-center text-[13px] font-medium text-text-primary">
        {username}
      </div>
    </div>
  );
});

/// A live screen share as a card in the tile row: 16:9 poster (the owner's
/// avatar — P2P calls have no thumbnails), LIVE pill, codec badge, and the
/// owner caption. Click → watch (the peer's) or preview (your own). Mirrors
/// the community voice view's stream cards so the two read the same.
function StreamCard({ info, owner, own }: { info: StreamInfo; owner: string; own: boolean }) {
  const label = own ? "Your screen" : `${owner}'s screen`;
  const open = () => void watchCallStream(owner);
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={open}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          open();
        }
      }}
      title={own ? "Preview what you're sharing" : `Watch ${owner}'s screen`}
      className="group mt-2 w-44 cursor-pointer overflow-hidden rounded-lg border border-border bg-bg-light transition-all duration-150 ease-out hover:border-accent/30 hover:shadow-float"
    >
      <div className="relative aspect-video w-full bg-bg-darkest">
        <CodecBadge
          codec={info.currentCodec}
          width={info.resolutionWidth}
          height={info.resolutionHeight}
          fps={info.fps}
          enforced={false}
          size="small"
        />
        <div className="flex h-full w-full items-center justify-center">
          <UserAvatar username={owner} size={40} />
        </div>
        <div className="absolute left-2 top-2 flex items-center gap-[5px] rounded-sm bg-error/90 px-1.5 py-0.5">
          <div className="h-1.5 w-1.5 animate-pulse rounded-full bg-white" />
          <span className="text-[10px] font-semibold text-white">LIVE</span>
        </div>
        <div className="absolute inset-0 flex items-center justify-center bg-black/0 transition-colors group-hover:bg-black/40">
          <span className="text-[12px] font-medium text-white opacity-0 transition-opacity group-hover:opacity-100">
            {own ? "Preview" : "Watch"}
          </span>
        </div>
      </div>
      <div className="flex items-center gap-2 px-2.5 py-2">
        <UserAvatar username={owner} size={20} />
        <div className="min-w-0 flex-1">
          <div className="truncate text-[12px] font-medium text-text-primary">{label}</div>
          {qualityLabel(info) && (
            <div className="truncate font-meta text-[10.5px] text-text-muted">{qualityLabel(info)}</div>
          )}
        </div>
      </div>
    </div>
  );
}

/// Small avatar + name chip over the video (bottom-right). Speaking ring,
/// mute glyph, right-click → the user menu.
const PeerChip = memo(function PeerChip({ username, muted }: { username: string; muted?: boolean }) {
  const isSpeaking = useVoiceStore((s) => s.speakingUsers.has(username));
  const peerMuted = useVoiceStore(
    (s) => s.participants.find((p) => p.username === username)?.isMuted ?? false,
  );
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  return (
    <div
      className="flex h-8 items-center gap-2 rounded-md border border-white/10 bg-black/70 pl-1 pr-2.5 text-[12px] font-medium text-text-primary"
      onContextMenu={(e) => {
        e.preventDefault();
        openContextMenu(username, { x: e.clientX, y: e.clientY }, null);
      }}
    >
      <div
        className={`rounded-md transition-all duration-150 ${
          isSpeaking ? "shadow-[0_0_0_2px_rgba(0,0,0,0.9),0_0_0_4px_var(--color-success)]" : ""
        }`}
      >
        <UserAvatar username={username} size={24} />
      </div>
      {username}
      {(muted || peerMuted) && (
        <svg className="h-3 w-3 text-error" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
          <line x1="1" y1="1" x2="23" y2="23" />
          <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
          <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
        </svg>
      )}
    </div>
  );
});

/// Local mute + volume for the peer, inline under their tile (the same
/// controls the right-click menu offers, made discoverable).
function PeerAudioControls({ username }: { username: string }) {
  const isLocallyMuted = useVoiceStore((s) => s.localMutedUsers.has(username));
  const currentDb = useVoiceStore((s) => s.userVolumes[username] ?? DEFAULT_DB);
  const setUserVolume = useVoiceStore((s) => s.setUserVolume);
  const toggleLocalMute = useVoiceStore((s) => s.toggleLocalMute);

  const sliderValue = ((currentDb - MIN_DB) / (MAX_DB - MIN_DB)) * 100;
  const handleSlider = (val: number) => {
    const raw = MIN_DB + (val / 100) * (MAX_DB - MIN_DB);
    const db = Math.abs(raw) < 0.8 ? 0 : Math.round(raw * 10) / 10;
    setUserVolume(username, Math.max(MIN_DB, Math.min(MAX_DB, db)));
    pushUserGain(username);
    saveSettings();
  };
  const handleToggleMute = () => {
    toggleLocalMute(username);
    pushUserGain(username);
    saveSettings();
  };

  return (
    <div className="mt-0.5 flex w-[10rem] items-center gap-1.5">
      <button
        onClick={handleToggleMute}
        title={isLocallyMuted ? `Unmute ${username} for you` : `Mute ${username} for you`}
        className={`flex h-6 w-6 shrink-0 items-center justify-center rounded-sm transition-colors ${
          isLocallyMuted
            ? "bg-error/15 text-error hover:bg-error/25"
            : "text-text-muted hover:bg-surface-hover hover:text-text-secondary"
        }`}
      >
        <VolumeIcon muted={isLocallyMuted} size={13} />
      </button>
      <input
        type="range"
        min={0}
        max={100}
        value={sliderValue}
        disabled={isLocallyMuted}
        onChange={(e) => handleSlider(Number(e.target.value))}
        title={`${formatDb(currentDb)} · ${dbToPercent(currentDb)}`}
        className="h-[3px] min-w-0 flex-1 cursor-pointer appearance-none rounded-full bg-bg-lighter accent-accent disabled:cursor-not-allowed disabled:opacity-40 [&::-webkit-slider-thumb]:h-[10px] [&::-webkit-slider-thumb]:w-[10px] [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-light"
      />
      <span className="w-9 shrink-0 text-right font-meta text-[10.5px] tabular-nums text-text-muted">
        {isLocallyMuted ? "Muted" : dbToPercent(currentDb)}
      </span>
    </div>
  );
}

// ── icons (Feather-style, stroke) ────────────────────────────────

const svgProps = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
};

function MicIcon() {
  return (
    <svg width="18" height="18" {...svgProps}>
      <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
      <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
      <line x1="12" y1="19" x2="12" y2="23" />
      <line x1="8" y1="23" x2="16" y2="23" />
    </svg>
  );
}
function MicOffIcon() {
  return (
    <svg width="18" height="18" {...svgProps}>
      <line x1="1" y1="1" x2="23" y2="23" />
      <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
      <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
      <line x1="12" y1="19" x2="12" y2="23" />
      <line x1="8" y1="23" x2="16" y2="23" />
    </svg>
  );
}
function DeafenIcon() {
  return (
    <svg width="18" height="18" {...svgProps}>
      <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
    </svg>
  );
}
function DeafenOffIcon() {
  return (
    <svg width="18" height="18" {...svgProps}>
      <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
      <line x1="1" y1="1" x2="23" y2="23" />
    </svg>
  );
}
function ShareIcon() {
  return (
    <svg width="18" height="18" {...svgProps}>
      <rect x="2" y="3" width="20" height="14" rx="2" ry="2" />
      <line x1="8" y1="21" x2="16" y2="21" />
      <line x1="12" y1="17" x2="12" y2="21" />
    </svg>
  );
}
function StopShareIcon() {
  return (
    <svg width="18" height="18" {...svgProps}>
      <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
      <rect x="8" y="8" width="8" height="8" rx="1" fill="currentColor" stroke="none" />
    </svg>
  );
}
function HangUpIcon() {
  return (
    <svg width="16" height="16" {...svgProps}>
      <path d="M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.42 19.42 0 0 1-3.33-2.67m-2.67-3.34a19.79 19.79 0 0 1-3.07-8.63A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91" />
      <line x1="23" y1="1" x2="1" y2="23" />
    </svg>
  );
}
function TheaterIcon() {
  return (
    <svg width="16" height="16" {...svgProps}>
      <polyline points="15 3 21 3 21 9" />
      <polyline points="9 21 3 21 3 15" />
      <line x1="21" y1="3" x2="14" y2="10" />
      <line x1="3" y1="21" x2="10" y2="14" />
    </svg>
  );
}
function CollapseIcon() {
  return (
    <svg width="16" height="16" {...svgProps}>
      <polyline points="4 14 10 14 10 20" />
      <polyline points="20 10 14 10 14 4" />
      <line x1="14" y1="10" x2="21" y2="3" />
      <line x1="3" y1="21" x2="10" y2="14" />
    </svg>
  );
}
function FullscreenIcon() {
  return (
    <svg width="16" height="16" {...svgProps}>
      <path d="M8 3H5a2 2 0 0 0-2 2v3m18 0V5a2 2 0 0 0-2-2h-3m0 18h3a2 2 0 0 0 2-2v-3M3 16v3a2 2 0 0 0 2 2h3" />
    </svg>
  );
}
function StatsIcon() {
  return (
    <svg width="16" height="16" {...svgProps}>
      <line x1="18" y1="20" x2="18" y2="10" />
      <line x1="12" y1="20" x2="12" y2="4" />
      <line x1="6" y1="20" x2="6" y2="14" />
    </svg>
  );
}
function CloseIcon() {
  return (
    <svg width="16" height="16" {...svgProps}>
      <line x1="18" y1="6" x2="6" y2="18" />
      <line x1="6" y1="6" x2="18" y2="18" />
    </svg>
  );
}
function VolumeIcon({ muted, size = 16 }: { muted: boolean; size?: number }) {
  return (
    <svg width={size} height={size} {...svgProps}>
      <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
      {muted ? (
        <>
          <line x1="23" y1="9" x2="17" y2="15" />
          <line x1="17" y1="9" x2="23" y2="15" />
        </>
      ) : (
        <>
          <path d="M19.07 4.93a10 10 0 0 1 0 14.14" />
          <path d="M15.54 8.46a5 5 0 0 1 0 7.07" />
        </>
      )}
    </svg>
  );
}
