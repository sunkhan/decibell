import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { LockGlyph } from "../chat/MessageBubble";
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
import { AVATAR_RADIUS } from "../../components/LetterAvatar";
import { playSound } from "../../utils/sounds";
import type { StreamInfo } from "../../types";
import CaptureSourcePicker from "../voice/CaptureSourcePicker";
import { StreamAudioButton } from "../voice/StreamAudioPopover";
import StreamStatsOverlay from "../voice/StreamStatsOverlay";
import StreamVideoPlayer from "../voice/StreamVideoPlayer";
import { toggleDeafen, toggleMute } from "../voice/voiceActions";
import { getStreamPipHost, placeStreamPip, recordFullViewRect } from "../voice/streamPipHost";
import { MAX_TILE_W, TILE_ASPECT, TILE_GAP, fitGrid, useElementSize } from "../voice/stage/stageLayout";
import {
  ACTION_PRIMARY,
  ACTION_SECONDARY,
  LivePill,
  OverlayButton,
  ParticipantTile,
  QualityPill,
  SCRIM,
  SCRIM_LG,
  VolumeControl,
} from "../voice/stage/StageTiles";
import {
  CloseIcon,
  CollapseIcon,
  ExpandIcon,
  EyeIcon,
  HeadphonesIcon,
  HeadphonesOffIcon,
  LeaveIcon,
  MicIcon,
  MicOffIcon,
  PlayIcon,
  ScreenIcon,
  SignalIcon,
  StatsIcon,
  StopShareIcon,
} from "../voice/stage/icons";
import { announceCallStreamStop, endCall, unwatchCallStream, watchCallStream } from "./callActions";
import { useUserName, type Uid } from "../../stores/usersStore";

/// The P2P call "stage": the top of the DM conversation while a call with
/// `peer` is ringing / connecting / active. It is the community voice view
/// at DM size (docs/superpowers/specs/2026-10-04-voice-view-redesign.md):
/// the same 16:9 tiles, stream tiles, scrims and dock, under a slim call
/// bar (status, encryption, ping, theater) that stands in for the voice
/// header, since the DM header is already above it.
///
///   tiles   → you and the peer as participant tiles, best-fit to the
///             stage, a stream tile per live screen share beside them (a
///             watched one plays in its tile)
///   focused → the stream fills the stage edge to edge; everything else
///             floats over it and fades with a still cursor (owner's call:
///             the video keeps all the room). A click goes back to the tiles
///   theater → the stage fills the DM panel (DmChatPanel hides the chat)
///   full    → window fullscreen, black, the same overlays, Esc exits
///
/// The video is the ONE persistent StreamVideoPlayer that StreamPipManager
/// owns: this component only claims its host node into a slot (the same
/// reparenting StreamViewPanel and the mini player do), so focusing a
/// stream here, backing out to its tile, minimising it to the mini player
/// and coming back never re-decodes. Which stream is "focused" is
/// `voiceStore.fullscreenStream` (the peer's, or our own self-preview).
export default function CallStage({ peer }: { peer: Uid }) {
  const status = useCallStore((s) => s.status);
  const callPeer = useCallStore((s) => s.peer);
  if (status === "idle" || status === "incoming" || callPeer !== peer) return null;
  return <Stage peer={peer} />;
}

/// The overlays over the video hide after the cursor has been still this
/// long (the voice view's focused stream does the same).
const OVERLAY_HIDE_MS = 2500;
// Chat keeps at least this much room under the compact stage.
const CHAT_MIN = 180;
/// One row of tiles wins unless the best-fit grid's tiles are this much
/// bigger (see CallTiles).
const ROW_PREFERENCE = 0.85;
/// Below these stage widths the focused view drops what would collide:
/// the people chips beside the dock, then the call status and the quality
/// pill in the top row.
const CHIPS_MIN_W = 940;
const STATUS_MIN_W = 700;
const QUALITY_MIN_W = 560;

// One handle for the module: getCurrentWindow() builds a fresh object per
// call, which made the fullscreen callbacks (and the effects keyed on them)
// change identity on every render.
const appWindow = getCurrentWindow();

/// The controls over the video swallow clicks and double-clicks, so using
/// them never reaches the video underneath (back to the tiles / exit
/// fullscreen).
const stopClick = (e: React.MouseEvent) => e.stopPropagation();

/// Fixed colours for things drawn over video, in every theme (the
/// CodecBadge / scrim rule): the theme's green can be too dark on black.
const ON_VIDEO_GREEN = "#65d097";

function formatDuration(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${mm}:${ss}` : `${mm}:${ss}`;
}

/// Back from the focused stream to the tiles; it keeps playing in its tile.
function backToTiles() {
  useVoiceStore.getState().setFullscreenStream(null);
}

function Stage({ peer }: { peer: Uid }) {
  const me = useAuthStore((s) => s.uid);
  const status = useCallStore((s) => s.status);
  const theater = useCallStore((s) => s.theater);
  const setTheater = useCallStore((s) => s.setTheater);
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
  const peerStream = activeStreams.find((st) => st.ownerUid === peer);
  const ownStream = isStreaming ? activeStreams.find((st) => st.ownerUid === me) : undefined;
  // The stream shown on the stage: the peer's, or our own preview.
  const focused =
    fullscreenStream != null &&
    (fullscreenStream === peer || fullscreenStream === me) &&
    watchingStreams.includes(fullscreenStream) &&
    activeStreams.some((st) => st.ownerUid === fullscreenStream)
      ? fullscreenStream
      : null;
  const focusedInfo = focused ? activeStreams.find((st) => st.ownerUid === focused) : undefined;
  const isFullscreen = isStreamFullscreen && focused != null;

  const [showPicker, setShowPicker] = useState(false);
  const [showStats, setShowStats] = useState(false);
  const [streamVolume, setStreamVolume] = useState(100);
  const [overlayVisible, setOverlayVisible] = useState(true);
  const overlayTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const slotRef = useRef<HTMLDivElement>(null);
  const rootRef = useRef<HTMLDivElement>(null);
  const { width: stageWidth } = useElementSize(rootRef);
  const [resizing, setResizing] = useState(false);

  // Claim the persistent player into our slot whenever a stream is focused
  // here (re-runs on focus change so we reclaim it from the mini player or
  // the stream's tile). On the way out, detach the host from our slot
  // explicitly: it was appended by hand, not by React, so if it ever stayed
  // behind inside a container React later reused for the tiles it would
  // sit in the flex row as a stray full-width child and squeeze the tiles.
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
  const enterFullscreen = useCallback(async () => {
    setStreamFullscreen(true);
    setOverlayVisible(true);
    await appWindow.setFullscreen(true).catch(() => {});
  }, [setStreamFullscreen]);
  const exitFullscreen = useCallback(async () => {
    setStreamFullscreen(false);
    setOverlayVisible(true);
    await appWindow.setFullscreen(false).catch(() => {});
  }, [setStreamFullscreen]);

  // Esc steps back one level: fullscreen → the focused stream → the tiles
  // (as in the voice view) → out of theater.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (isFullscreen) void exitFullscreen();
      else if (focused) backToTiles();
      else if (theater) setTheater(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [isFullscreen, exitFullscreen, focused, theater, setTheater]);

  // The focused stream ended (or was unwatched) while fullscreen → come back.
  useEffect(() => {
    if (isStreamFullscreen && !focused) void exitFullscreen();
  }, [isStreamFullscreen, focused, exitFullscreen]);

  // The overlays over the video show on mouse move, hide after a still
  // cursor or when it leaves, and stay while it rests on a control.
  const pokeOverlay = useCallback(() => {
    setOverlayVisible(true);
    if (overlayTimer.current) clearTimeout(overlayTimer.current);
    overlayTimer.current = setTimeout(() => setOverlayVisible(false), OVERLAY_HIDE_MS);
  }, []);
  const holdOverlay = useCallback(() => {
    if (overlayTimer.current) clearTimeout(overlayTimer.current);
    setOverlayVisible(true);
  }, []);
  const hideOverlay = useCallback(() => {
    if (overlayTimer.current) clearTimeout(overlayTimer.current);
    setOverlayVisible(false);
  }, []);
  useEffect(() => {
    pokeOverlay();
    return () => {
      if (overlayTimer.current) clearTimeout(overlayTimer.current);
    };
  }, [focused, isFullscreen, pokeOverlay]);

  // ── handlers ──
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

  // ── resize (compact stage only): drag the bottom edge ──
  // Imperative during the drag (no per-frame React state), committed to
  // uiStore on release so it persists per mode. The chat below keeps at
  // least CHAT_MIN px; double-click resets the mode's default.
  const compact = !isFullscreen && !theater;
  const mode: "voice" | "stream" = focused ? "stream" : "voice";
  const stageHeight = mode === "stream" ? streamHeight : voiceHeight;
  const maxStageHeight = () => {
    const parent = rootRef.current?.parentElement;
    return parent ? Math.max(CALL_STAGE_MIN, parent.clientHeight - 48 - CHAT_MIN) : Infinity;
  };
  // The compact height cap depends on the parent's height. Track it with a
  // ResizeObserver instead of reading clientHeight during render (a forced
  // layout on every render — and the stage used to re-render every second).
  const [parentHeight, setParentHeight] = useState<number | null>(null);
  useLayoutEffect(() => {
    const parent = rootRef.current?.parentElement;
    if (!parent) return;
    setParentHeight(parent.clientHeight);
    const ro = new ResizeObserver(() => setParentHeight(parent.clientHeight));
    ro.observe(parent);
    return () => ro.disconnect();
  }, []);
  const maxCompactHeight =
    parentHeight != null ? Math.max(CALL_STAGE_MIN, parentHeight - 48 - CHAT_MIN) : Infinity;
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
    ? "fixed inset-0 z-50 flex flex-col bg-black"
    : theater
      ? "relative flex min-h-0 flex-1 flex-col border-b border-border bg-bg-dark"
      : "relative flex shrink-0 flex-col border-b border-border bg-bg-dark";
  const rootStyle = compact
    ? { height: Math.min(stageHeight, maxCompactHeight), userSelect: resizing ? ("none" as const) : undefined }
    : undefined;
  const overlayClass = `transition-opacity duration-150 ${
    overlayVisible ? "opacity-100" : "pointer-events-none opacity-0"
  }`;
  const peerVolume =
    focused != null && focused !== me ? (
      <VolumeControl
        volume={streamVolume}
        onChange={handleStreamVolume}
        onToggleMute={() => handleStreamVolume(streamVolume > 0 ? 0 : 100)}
      />
    ) : null;

  return (
    <div ref={rootRef} className={rootClass} style={rootStyle}>
      {/* Distinct keys: the video and tiles containers are both a <div> in the
          same position, so without keys React would reuse one DOM node for the
          other and the hand-parented player host could survive the switch. */}
      {focused && focusedInfo ? (
        <div
          key="video"
          className={`relative flex min-h-0 min-w-0 flex-1 items-center justify-center overflow-hidden bg-black ${
            isFullscreen ? (overlayVisible ? "cursor-default" : "cursor-none") : "cursor-pointer"
          }`}
          onClick={isFullscreen ? undefined : backToTiles}
          onDoubleClick={isFullscreen ? () => void exitFullscreen() : undefined}
          onMouseMove={pokeOverlay}
          onMouseLeave={hideOverlay}
        >
          <div ref={slotRef} className="h-full w-full" />
          {showStats && <StreamStatsOverlay uid={focused} className="left-3 top-14" />}

          {/* top-left: what this is */}
          <div className={`pointer-events-none absolute left-3 top-3 flex gap-1.5 ${overlayClass}`}>
            <LivePill size="lg" />
            <StreamOwnerChip info={focusedInfo} own={focused === me} />
            {stageWidth >= QUALITY_MIN_W && <QualityPill stream={focusedInfo} large />}
          </div>

          {/* top-right: the call, then the view controls */}
          <div
            className={`absolute right-3 top-3 flex items-center gap-1.5 ${overlayClass}`}
            onMouseEnter={holdOverlay}
            onClick={stopClick}
            onDoubleClick={stopClick}
          >
            {isFullscreen && <span className="mr-1.5 text-[11px] text-white/50">Esc to exit</span>}
            {live && stageWidth >= STATUS_MIN_W && <CallStatusOnVideo peer={peer} />}
            <OverlayButton title="Stream stats" active={showStats} onClick={() => setShowStats((v) => !v)}>
              <StatsIcon size={16} />
            </OverlayButton>
            {!isFullscreen && (
              <OverlayButton title={theater ? "Exit theater" : "Theater"} active={theater} onClick={toggleTheater}>
                {theater ? <CollapseIcon size={16} /> : <TheaterIcon size={16} />}
              </OverlayButton>
            )}
            <OverlayButton
              title={isFullscreen ? "Exit fullscreen" : "Fullscreen"}
              onClick={() => void (isFullscreen ? exitFullscreen() : enterFullscreen())}
            >
              {isFullscreen ? <CollapseIcon size={16} /> : <ExpandIcon size={16} />}
            </OverlayButton>
            <OverlayButton title={focused === me ? "Close preview" : "Stop watching"} onClick={handleStopWatching}>
              <CloseIcon size={16} />
            </OverlayButton>
          </div>

          {/* bottom: the dock on a scrim, the stream volume in it */}
          <div className={`pointer-events-none absolute inset-x-0 bottom-4 flex justify-center ${overlayClass}`}>
            <div className="pointer-events-auto" onMouseEnter={holdOverlay} onClick={stopClick} onDoubleClick={stopClick}>
              <CallDock
                status={status}
                overVideo
                volume={peerVolume}
                onShare={() => setShowPicker(true)}
                onStopShare={() => void handleStopSharing()}
                onHangUp={handleHangUp}
              />
            </div>
          </div>

          {/* bottom-right: who's talking */}
          {stageWidth >= CHIPS_MIN_W && (
            <div
              className={`absolute bottom-6.5 right-3 flex gap-1.5 ${overlayClass}`}
              onMouseEnter={holdOverlay}
              onClick={stopClick}
              onDoubleClick={stopClick}
            >
              <PeerChip uid={me} isLocal />
              <PeerChip uid={peer} isLocal={false} />
            </div>
          )}
        </div>
      ) : (
        <>
          <CallBar peer={peer} live={live} theater={theater} onToggleTheater={toggleTheater} />
          <CallTiles key="tiles" peer={peer} me={me} live={live} peerStream={peerStream} ownStream={ownStream} />
          <div className="pointer-events-none absolute inset-x-0 bottom-4 z-10 flex justify-center">
            <div className="pointer-events-auto">
              <CallDock
                status={status}
                overVideo={false}
                volume={null}
                onShare={() => setShowPicker(true)}
                onStopShare={() => void handleStopSharing()}
                onHangUp={handleHangUp}
              />
            </div>
          </div>
        </>
      )}

      {/* ── bottom edge: drag to resize (compact only) ── */}
      {compact && (
        <div
          onPointerDown={onResizeStart}
          onDoubleClick={onResizeReset}
          title="Drag to resize · double-click to reset"
          className="group absolute inset-x-0 -bottom-1 z-20 flex h-2.5 cursor-ns-resize items-center justify-center"
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

// ── call bar ─────────────────────────────────────────────────────

/// The voice header at DM size: call state and duration, the encryption
/// badge, the voice ping and path, and the theater toggle. Same type as the
/// voice header's stats (font-meta, tabular digits).
function CallBar({
  peer,
  live,
  theater,
  onToggleTheater,
}: {
  peer: Uid;
  live: boolean;
  theater: boolean;
  onToggleTheater: () => void;
}) {
  const verified = useCallStore((s) => s.verified);
  const error = useVoiceStore((s) => s.error);
  const peerName = useUserName(peer, "Unknown user");
  return (
    <div className="flex h-11 shrink-0 items-center gap-2.5 px-4">
      <CallStatus />
      {live && verified !== null && (
        <span
          title={verifiedTitle(verified, peerName)}
          className={`flex h-5.5 shrink-0 items-center gap-1.5 rounded-sm px-2 font-meta text-micro font-medium ${
            verified ? "bg-success/15 text-success" : "bg-text-muted/15 text-text-muted"
          }`}
        >
          <LockGlyph size={11} />
          {verified ? "Verified" : "Not verified"}
        </span>
      )}
      {error && live && (
        <span className="truncate rounded-sm bg-error/15 px-2 py-0.5 font-meta text-micro text-error">{error}</span>
      )}
      <div className="flex-1" />
      {live && <CallStats />}
      {live && (
        <button
          type="button"
          onClick={onToggleTheater}
          title={theater ? "Exit theater (Esc)" : "Theater"}
          aria-label={theater ? "Exit theater" : "Theater"}
          aria-pressed={theater}
          className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-sm transition-colors ${
            theater ? "bg-accent-soft text-accent" : "text-text-muted hover:bg-surface-hover hover:text-text-secondary"
          }`}
        >
          {theater ? <CollapseIcon size={15} /> : <TheaterIcon size={15} />}
        </button>
      )}
    </div>
  );
}

function verifiedTitle(verified: boolean, peerName: string): string {
  return verified
    ? `End-to-end encrypted. ${peerName}'s call key matches their verified identity.`
    : `End-to-end encrypted, but ${peerName} hasn't set up encryption, so their identity can't be checked.`;
}

/// The running duration, ticking once a second. Its own component so the
/// tick re-renders only this text.
function CallDuration() {
  const startedAt = useCallStore((s) => s.startedAt);
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    setNow(Date.now());
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, []);
  return <>{startedAt ? formatDuration(now - startedAt) : ""}</>;
}

/// "Calling…" / "Ringing…" / "Connecting…" with a breathing dot, or the
/// green dot, "Voice call" and the running duration once live.
function CallStatus() {
  const status = useCallStore((s) => s.status);
  const ringingAcked = useCallStore((s) => s.ringingAcked);
  const live = status === "active";
  const pending = status === "outgoing" ? (ringingAcked ? "Ringing…" : "Calling…") : "Connecting…";
  return (
    <span className="flex shrink-0 items-center gap-2 font-meta text-meta font-medium tabular-nums text-text-secondary">
      {live ? (
        <span className="h-2 w-2 rounded-full bg-success" />
      ) : (
        <span className="h-2 w-2 animate-[breathe_1.6s_ease-in-out_infinite] rounded-full bg-accent" />
      )}
      {live ? "Voice call" : pending}
      {live && (
        <>
          <span className="h-3.5 w-px bg-border-divider" />
          <span className="text-text-muted">
            <CallDuration />
          </span>
        </>
      )}
    </span>
  );
}

/// Ping (toned like the voice header's) and how the call is connected.
/// Owns the 3 s ping subscription.
function CallStats() {
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const connectedPath = useCallStore((s) => s.connectedPath);
  const tone =
    latencyMs == null ? "" : latencyMs <= 70 ? "text-success" : latencyMs < 175 ? "text-warning" : "text-error";
  const path = connectedPath === "host" ? "LAN" : connectedPath === "srflx" ? "Direct" : null;
  if (latencyMs == null && !path) return null;
  return (
    <div className="flex shrink-0 items-center gap-1 font-meta text-meta tabular-nums text-text-muted">
      {latencyMs != null && (
        <span className={`flex h-7 items-center gap-1.5 px-1.5 ${tone}`} title="Voice latency">
          <SignalIcon />
          {latencyMs} ms
        </span>
      )}
      {latencyMs != null && path && <span className="h-4 w-px bg-border-divider" />}
      {path && (
        <span
          className="flex h-7 items-center px-1.5"
          title={path === "LAN" ? "Connected over your local network" : "Connected directly, peer to peer"}
        >
          {path}
        </span>
      )}
    </div>
  );
}

/// The call bar's facts on a video scrim, for the focused view (which gives
/// the video the whole stage): duration, ping, and the lock, green when
/// verified.
function CallStatusOnVideo({ peer }: { peer: Uid }) {
  const verified = useCallStore((s) => s.verified);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const peerName = useUserName(peer, "Unknown user");
  return (
    <span className={`${SCRIM_LG} tabular-nums`}>
      <span className="h-2 w-2 rounded-full" style={{ background: ON_VIDEO_GREEN }} />
      <CallDuration />
      {latencyMs != null && <span className="text-white/60">{latencyMs} ms</span>}
      {verified !== null && (
        <span
          title={verifiedTitle(verified, peerName)}
          aria-label={verified ? "Verified" : "Not verified"}
          className={verified ? "" : "text-white/45"}
          style={verified ? { color: ON_VIDEO_GREEN } : undefined}
        >
          <LockGlyph size={11} />
        </span>
      )}
    </span>
  );
}

// ── tiles ────────────────────────────────────────────────────────

/// You, the peer and any live screen share as one row of 16:9 tiles,
/// kept clear of the floating dock (pb-20: the dock's 52 px, the 16 px it
/// floats above the edge, and air, as VoicePanel's pb-24 does).
function CallTiles({
  peer,
  me,
  live,
  peerStream,
  ownStream,
}: {
  peer: Uid;
  me: Uid;
  live: boolean;
  peerStream: StreamInfo | undefined;
  ownStream: StreamInfo | undefined;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const { width, height } = useElementSize(ref);
  const streams = live ? [peerStream, ownStream].filter((s): s is StreamInfo => !!s) : [];
  const n = 2 + streams.length;
  const best = fitGrid(n, width, height, TILE_GAP, MAX_TILE_W);
  // Side by side reads as a conversation: one row unless stacking gives
  // clearly bigger tiles (a tall theater panel otherwise stacks the two of
  // you for a few percent).
  const rowW = Math.floor(Math.min((width - TILE_GAP * (n - 1)) / n, height * TILE_ASPECT, MAX_TILE_W));
  const fit = rowW >= best.tileW * ROW_PREFERENCE ? { cols: n, tileW: rowW } : best;
  const measured = width > 0 && height > 0;
  return (
    <div ref={ref} className="relative min-h-0 flex-1 px-6 pb-20 pt-1">
      {measured && (
        <div className="flex h-full items-center justify-center">
          <div
            className="flex flex-wrap justify-center gap-3"
            style={{ maxWidth: fit.cols * fit.tileW + (fit.cols - 1) * TILE_GAP }}
          >
            <CallParticipant uid={me} isLocal live={live} width={fit.tileW} />
            <CallParticipant uid={peer} isLocal={false} live={live} width={fit.tileW} />
            {streams.map((s) => (
              <CallStreamTile key={s.streamId} info={s} own={s.ownerUid === me} width={fit.tileW} />
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

/// A voice-view participant tile for one side of the call (right-click →
/// the user menu: volume, mute for you, as in voice channels). While the
/// call rings or connects, the peer's tile is dimmed under a breathing
/// accent outline; once live it brightens in place.
const CallParticipant = memo(function CallParticipant({
  uid,
  isLocal,
  live,
  width,
}: {
  uid: Uid;
  isLocal: boolean;
  live: boolean;
  width: number;
}) {
  const p = useVoiceStore((s) => s.participants.find((x) => x.uid === uid));
  const username = useUserName(uid, p?.username ?? "Unknown user");
  const waiting = !isLocal && !live;
  return (
    <div className="relative shrink-0" style={{ width }}>
      <div className={`transition-opacity duration-300 ${waiting ? "opacity-55" : ""}`}>
        <ParticipantTile
          uid={uid}
          username={username}
          isLocal={isLocal}
          rosterMuted={p?.isMuted ?? false}
          rosterDeafened={p?.isDeafened ?? false}
          connectedServerId={null}
          width={width}
        />
      </div>
      {waiting && (
        <span
          aria-hidden
          className="pointer-events-none absolute inset-0 animate-[breathe_1.6s_ease-in-out_infinite] rounded-lg outline-2 -outline-offset-2 outline-accent"
        />
      )}
    </div>
  );
});

/// "mira's screen" with the owner's avatar, on the video scrim.
function StreamOwnerChip({ info, own }: { info: StreamInfo; own: boolean }) {
  const name = useUserName(info.ownerUid, info.ownerUsername);
  return (
    <span className={SCRIM_LG}>
      <UserAvatar uid={info.ownerUid} name={name} size={18} />
      {own ? "Your screen" : `${name}'s screen`}
    </span>
  );
}

/// A live screen share as a voice-view stream tile: LIVE, quality, the
/// owner on a scrim, and the actions on hover. Not watched: Watch stream
/// (or Preview for your own), which opens it on the stage. Watched (backed
/// out of the focused view): it plays here, through the persistent player
/// when that's the stream it holds, with Focus and Stop. P2P calls carry no
/// thumbnails, so an unwatched tile shows the owner's avatar.
const CallStreamTile = memo(function CallStreamTile({
  info,
  own,
  width,
}: {
  info: StreamInfo;
  own: boolean;
  width: number;
}) {
  const owner = info.ownerUid;
  const ownerName = useUserName(owner, info.ownerUsername);
  const isWatching = useVoiceStore((s) => s.watchingStreams.includes(owner));
  const isPip = useVoiceStore((s) => s.pipStream === owner);
  const label = own ? "Your screen" : `${ownerName}'s screen`;

  // The stream the persistent player already decodes is shown by moving that
  // player's host in here (as the focused view and the mini player do)
  // rather than starting a second decoder for the same stream.
  const slotRef = useRef<HTMLDivElement>(null);
  const hostsPip = isWatching && isPip;
  useLayoutEffect(() => {
    const slot = slotRef.current;
    if (!hostsPip || !slot) return;
    placeStreamPip(slot);
    return () => {
      const host = getStreamPipHost();
      if (host.parentElement === slot) host.remove();
    };
  }, [hostsPip]);

  const focus = () => useVoiceStore.getState().setFullscreenStream(owner);
  const primary = () => (isWatching ? focus() : void watchCallStream(owner));
  const act = (run: () => void) => (e: React.MouseEvent) => {
    e.stopPropagation();
    run();
  };
  return (
    <div
      role="button"
      tabIndex={0}
      aria-label={label}
      title={label}
      onClick={primary}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          primary();
        }
      }}
      className={`group relative aspect-video shrink-0 cursor-pointer overflow-hidden rounded-lg border border-border bg-black shadow-raised ${
        isWatching ? "outline-2 -outline-offset-2 outline-accent" : ""
      }`}
      style={{ width }}
    >
      <div className="absolute inset-0 flex items-center justify-center">
        {hostsPip ? (
          <div ref={slotRef} className="h-full w-full" />
        ) : isWatching ? (
          <StreamVideoPlayer streamerUid={owner} className="h-full w-full object-cover" />
        ) : (
          <UserAvatar uid={owner} name={ownerName} size={56} />
        )}
      </div>
      <div className="absolute inset-0 flex items-center justify-center gap-2 bg-black/45 opacity-0 group-focus-within:opacity-100 group-hover:opacity-100">
        {isWatching ? (
          <>
            <button type="button" className={ACTION_PRIMARY} onClick={act(focus)}>
              <ExpandIcon size={14} />
              Focus
            </button>
            <button type="button" className={ACTION_SECONDARY} onClick={act(() => void unwatchCallStream(owner))}>
              <CloseIcon size={14} bold />
              Stop
            </button>
          </>
        ) : (
          <button type="button" className={ACTION_PRIMARY} onClick={act(() => void watchCallStream(owner))}>
            <PlayIcon />
            {own ? "Preview" : "Watch stream"}
          </button>
        )}
      </div>
      <div className="absolute left-2 top-2 flex gap-1.5">
        <LivePill />
        {isWatching && (
          <span className="flex h-5 items-center gap-1 rounded-sm bg-accent px-1.5 font-meta text-micro font-semibold leading-none text-on-accent">
            <EyeIcon size={12} bold />
            Watching
          </span>
        )}
      </div>
      <QualityPill stream={info} className="absolute right-2 top-2" />
      <div className="absolute bottom-2 left-2 flex max-w-[calc(100%-1rem)]">
        <span className={`${SCRIM} min-w-0`}>
          <UserAvatar uid={owner} name={ownerName} size={16} />
          <span className="truncate">{label}</span>
        </span>
      </div>
    </div>
  );
});

/// One side of the call over the video: avatar with the speaking ring,
/// name, mute / deafen. Right-click → the user menu.
const PeerChip = memo(function PeerChip({ uid, isLocal }: { uid: Uid; isLocal: boolean }) {
  const isSpeaking = useVoiceStore((s) => s.speakingUsers.has(uid));
  const p = useVoiceStore((s) => s.participants.find((x) => x.uid === uid));
  const selfMuted = useVoiceStore((s) => isLocal && s.isMuted);
  const selfDeafened = useVoiceStore((s) => isLocal && s.isDeafened);
  const username = useUserName(uid, p?.username ?? "Unknown user");
  const muted = isLocal ? selfMuted : (p?.isMuted ?? false);
  const deafened = isLocal ? selfDeafened : (p?.isDeafened ?? false);
  return (
    <span
      className={`${SCRIM_LG} pl-1`}
      onContextMenu={(e) => {
        e.preventDefault();
        useUiStore.getState().openContextMenu(uid, { x: e.clientX, y: e.clientY }, null);
      }}
    >
      {/* Instant ring, no transition (the voice tiles' rule). */}
      <span
        className={isSpeaking ? "outline-2 outline-offset-1" : ""}
        style={{ borderRadius: AVATAR_RADIUS, outlineColor: isSpeaking ? ON_VIDEO_GREEN : undefined }}
      >
        <UserAvatar uid={uid} name={username} size={22} />
      </span>
      {username}
      {deafened ? (
        <span title="Deafened" className="text-error">
          <HeadphonesOffIcon size={13} bold />
        </span>
      ) : muted ? (
        <span title="Muted" className="text-error">
          <MicOffIcon size={13} bold />
        </span>
      ) : null}
    </span>
  );
});

// ── dock ─────────────────────────────────────────────────────────

const DOCK_BUTTON = "flex h-10 w-10 items-center justify-center rounded-md";
/// Themed tones (VoicePanel's dock) and their over-video twins (the voice
/// view's fullscreen dock).
const DOCK_TONES = {
  soft: "bg-surface-hover text-text-secondary hover:bg-surface-active hover:text-text-bright",
  danger: "bg-error/15 text-error hover:bg-error/25",
  on: "bg-accent text-on-accent hover:bg-accent-hover",
} as const;
const VIDEO_TONES = {
  soft: "text-white/80 hover:bg-white/10 hover:text-white",
  danger: "bg-error/20 text-error hover:bg-error/30",
  on: "bg-accent text-on-accent hover:bg-accent-hover",
} as const;

/// The voice dock with the call's ending: mic and headphones │ screen share
/// (+ the stream-audio app picker) │ the watched stream's volume, over video
/// │ end call. Until the call is live it is only the red button, labelled
/// Cancel / Hang up.
function CallDock({
  status,
  overVideo,
  volume,
  onShare,
  onStopShare,
  onHangUp,
}: {
  status: string;
  overVideo: boolean;
  volume: React.ReactNode;
  onShare: () => void;
  onStopShare: () => void;
  onHangUp: () => void;
}) {
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const isStreaming = useVoiceStore((s) => s.isStreaming);
  const live = status === "active";
  const tones = overVideo ? VIDEO_TONES : DOCK_TONES;
  const divider = <div className={`mx-0.5 h-6 w-px ${overVideo ? "bg-white/10" : "bg-border-divider"}`} />;
  const button = (title: string, tone: keyof typeof DOCK_TONES, onClick: () => void, icon: React.ReactNode) => (
    <button type="button" title={title} aria-label={title} onClick={onClick} className={`${DOCK_BUTTON} ${tones[tone]}`}>
      {icon}
    </button>
  );
  const endLabel = status === "outgoing" ? "Cancel" : "Hang up";
  return (
    <div
      className={`flex items-center gap-1.5 rounded-lg border p-1.5 shadow-float ${
        overVideo ? "border-white/10 bg-black/70" : "border-border bg-bg-light"
      }`}
    >
      {live && (
        <>
          {button(isMuted ? "Unmute" : "Mute", isMuted ? "danger" : "soft", toggleMute, isMuted ? <MicOffIcon /> : <MicIcon />)}
          {button(
            isDeafened ? "Undeafen" : "Deafen",
            isDeafened ? "danger" : "soft",
            toggleDeafen,
            isDeafened ? <HeadphonesOffIcon /> : <HeadphonesIcon />,
          )}
          {divider}
          {button(
            isStreaming ? "Stop sharing" : "Share your screen",
            isStreaming ? "on" : "soft",
            isStreaming ? onStopShare : onShare,
            isStreaming ? <StopShareIcon /> : <ScreenIcon />,
          )}
          <StreamAudioButton size={18} className={`${DOCK_BUTTON} ${tones.soft}`} />
          {divider}
          {volume && (
            <>
              {volume}
              {divider}
            </>
          )}
        </>
      )}
      <button
        type="button"
        title={endLabel}
        aria-label={endLabel}
        onClick={onHangUp}
        className={`flex h-10 items-center justify-center gap-2 rounded-md bg-error text-on-error hover:bg-error/85 ${
          live ? "w-14" : "px-4 font-channel text-[13px] font-semibold"
        }`}
      >
        <LeaveIcon />
        {!live && endLabel}
      </button>
    </div>
  );
}

/// Theater: the stage takes the whole DM panel (maximize-2).
function TheaterIcon({ size }: { size: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="shrink-0">
      <polyline points="15 3 21 3 21 9" />
      <polyline points="9 21 3 21 3 15" />
      <line x1="21" y1="3" x2="14" y2="10" />
      <line x1="3" y1="21" x2="10" y2="14" />
    </svg>
  );
}
