import {
  useState,
  useRef,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  memo,
} from "react";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";
import { useDisplayName } from "../../hooks/useDisplayName";
import { invoke } from "../../lib/ipc";
import { getCurrentWindow } from "../../lib/window";
import { playSound } from "../../utils/sounds";
import { UserAvatar } from "../../components/UserAvatar";
import StreamStatsOverlay from "./StreamStatsOverlay";
import {
  getMiniRect,
  getStreamPipHost,
  placeStreamPip,
  recordFullViewRect,
} from "./streamPipHost";
import {
  STRIP_GAP,
  STRIP_TILE_W,
  rowCapacity,
  useElementSize,
  useVisibleParticipants,
} from "./stage/stageLayout";
import { LivePill, OverflowTile, ParticipantTile, QualityPill, SCRIM_BASE, SCRIM_LG, StreamTile } from "./stage/StageTiles";
import {
  CloseIcon,
  CollapseIcon,
  ExpandIcon,
  GridIcon,
  HeadphonesIcon,
  HeadphonesOffIcon,
  MicIcon,
  MicOffIcon,
  SpeakerIcon,
  StatsIcon,
} from "./stage/icons";

// One handle for the panel's lifetime: getCurrentWindow() builds a fresh
// object per call, which made the fullscreen callbacks (and the Escape /
// stream-ended effects keyed on them) change identity on every render.
const appWindow = getCurrentWindow();

const EMPTY_CHANNELS: never[] = [];
/// Double-click on the video toggles fullscreen; the controls over it eat
/// theirs so pressing a button twice quickly doesn't.
const stopDouble = (e: React.MouseEvent) => e.stopPropagation();
/// Overlays hide after the cursor has been still this long.
const OVERLAY_HIDE_MS = 2500;

/// Square icon button over video: fixed dark scrim, white glyph.
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
      type="button"
      title={title}
      aria-label={title}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className={`flex h-8 w-8 items-center justify-center rounded-sm border border-white/10 ${
        active ? "bg-accent text-on-accent" : "bg-black/70 text-white hover:bg-black/85"
      }`}
    >
      {children}
    </button>
  );
}

/// Stream volume on a scrim: mute toggle, slider (accent fill), percent.
function VolumeControl({
  volume,
  onChange,
  onToggleMute,
}: {
  volume: number;
  onChange: (v: number) => void;
  onToggleMute: () => void;
}) {
  return (
    <div className={`${SCRIM_BASE} h-8 gap-2.5 px-2.5`} onClick={(e) => e.stopPropagation()}>
      <button
        type="button"
        onClick={onToggleMute}
        title={volume > 0 ? "Mute stream" : "Unmute stream"}
        aria-label={volume > 0 ? "Mute stream" : "Unmute stream"}
        className={volume === 0 ? "text-error" : "text-white hover:text-white/80"}
      >
        <SpeakerIcon muted={volume === 0} size={15} />
      </button>
      <input
        type="range"
        min={0}
        max={100}
        value={volume}
        onChange={(e) => onChange(Number(e.target.value))}
        title={`Stream volume: ${volume}%`}
        className="custom-slider h-1 w-21 cursor-pointer appearance-none rounded-full [--slider-ring:#000]"
        style={{
          background: `linear-gradient(to right, var(--color-accent) ${volume}%, rgb(255 255 255 / 0.22) ${volume}%)`,
        }}
      />
      <span className="w-8 text-white/70">{volume}%</span>
    </div>
  );
}

function StreamViewPanel() {
  const fullscreenStream = useVoiceStore((s) => s.fullscreenStream);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);

  const currentUsername = useAuthStore((s) => s.username);
  const isFullscreen = useVoiceStore((s) => s.isStreamFullscreen);
  const setIsFullscreen = useVoiceStore((s) => s.setStreamFullscreen);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const [overlayVisible, setOverlayVisible] = useState(false);
  const overlayTimeout = useRef<ReturnType<typeof setTimeout>>(undefined);
  const [streamVolume, setStreamVolume] = useState(100);
  const prevVolume = useRef(100);
  const [showStats, setShowStats] = useState(false);
  const pipSlotRef = useRef<HTMLDivElement>(null);

  const [lastStreamUser, setLastStreamUser] = useState<string | null>(null);
  useEffect(() => {
    if (fullscreenStream) setLastStreamUser(fullscreenStream);
  }, [fullscreenStream]);

  const displayUser = fullscreenStream || lastStreamUser;
  const displayName = useDisplayName(connectedServerId, displayUser ?? "");

  // Claim the shared, persistent stream player node into the full-view slot
  // whenever a stream is focused here. The player is reparented (not remounted)
  // between this view, the grid's stream tile and the floating mini player, so
  // the decoder survives the move and playback is seamless. Re-runs on stream
  // switch so the full view reclaims the host after the mini player had it.
  // Also record the slot's rect so the mini player can shrink out of it.
  //
  // On the way out (back to the grid, stream switch, unmount) detach the host
  // from our slot, like CallStage does. "Back" doesn't unmount this panel —
  // VoicePanel only hides it (display:none) while any stream is watched — so a
  // host left in the slot kept the canvas connected and painting into an
  // invisible box. The grid's tile for this stream claims it next.
  useLayoutEffect(() => {
    const slot = pipSlotRef.current;
    if (fullscreenStream && slot) {
      placeStreamPip(slot);
      recordFullViewRect(slot);
    }
    return () => {
      const host = getStreamPipHost();
      if (slot && host.parentElement === slot) host.remove();
    };
  }, [fullscreenStream]);

  // On mount (returning to the voice view), grow the video back out of the mini
  // player's last spot. Mount-only: switching streams while already here must
  // not re-trigger it. Animates the slot's transform (the video inside is
  // pointer-events:none), leaving the surrounding controls untouched.
  useLayoutEffect(() => {
    const el = pipSlotRef.current;
    const from = getMiniRect();
    if (!el || !fullscreenStream || !from || from.width < 1) return;
    // Reset any leftover transform BEFORE measuring, so `to` is the true resting
    // rect. Without this, React StrictMode's double-invoke (run #1 sets the
    // transform, its cleanup cancels the animation but leaves the transform on)
    // makes run #2 measure the already-shrunk rect → no-op → stuck small.
    el.style.transition = "none";
    el.style.transform = "";
    const to = el.getBoundingClientRect();
    if (to.width < 1) return;
    const dx = from.left - to.left;
    const dy = from.top - to.top;
    const sx = from.width / to.width;
    const sy = from.height / to.height;
    if (Math.abs(dx) < 2 && Math.abs(dy) < 2 && Math.abs(sx - 1) < 0.02) return;
    el.style.transformOrigin = "top left";
    el.style.transform = `translate(${dx}px, ${dy}px) scale(${sx}, ${sy})`;
    void el.offsetWidth;
    const raf = requestAnimationFrame(() => {
      el.style.transition = "transform 340ms cubic-bezier(0.34, 1.32, 0.64, 1)";
      el.style.transform = "translate(0px, 0px) scale(1, 1)";
    });
    const done = setTimeout(() => {
      el.style.transition = "";
      el.style.transform = "";
    }, 440);
    return () => {
      cancelAnimationFrame(raf);
      clearTimeout(done);
      // Never leave the slot shrunk if torn down / re-invoked mid-animation.
      el.style.transition = "";
      el.style.transform = "";
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const enterFullscreen = useCallback(async () => {
    setIsFullscreen(true);
    await appWindow.setFullscreen(true).catch(() => {});
  }, [setIsFullscreen]);

  const exitFullscreen = useCallback(async () => {
    setIsFullscreen(false);
    setOverlayVisible(false);
    await appWindow.setFullscreen(false).catch(() => {});
  }, [setIsFullscreen]);

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
    const streamStillActive = activeStreams.some(
      (s) => s.ownerUsername === displayUser,
    );
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

  const stream = activeStreams.find((s) => s.ownerUsername === displayUser);
  const isOwnStream = displayUser === currentUsername;

  // Keep the latest slider value reachable from the mute effect's cleanup
  // without re-running that effect on every volume tick.
  const streamVolumeRef = useRef(streamVolume);
  useEffect(() => {
    streamVolumeRef.current = streamVolume;
  }, [streamVolume]);

  useEffect(() => {
    if (!isOwnStream) return;
    // Mute the (global) native stream-audio gain while previewing our own
    // stream so we don't hear our own shared audio echoed back.
    invoke("set_stream_volume", { volume: 0 }).catch(() => {});
    return () => {
      // Restore when we stop previewing our own stream (switching to
      // someone else's) or unmount — otherwise every subsequently watched
      // stream stays silent while the slider still shows its old value.
      invoke("set_stream_volume", {
        volume: streamVolumeRef.current / 100,
      }).catch(() => {});
    };
  }, [isOwnStream]);

  // Overlays (pills, buttons, volume) show while the cursor moves over the
  // video and fade after it rests — in the panel and in fullscreen alike.
  const pokeOverlay = useCallback(() => {
    setOverlayVisible(true);
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    overlayTimeout.current = setTimeout(() => setOverlayVisible(false), OVERLAY_HIDE_MS);
  }, []);
  const holdOverlay = useCallback(() => {
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    setOverlayVisible(true);
  }, []);
  const handleMouseLeave = useCallback(() => {
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    setOverlayVisible(false);
  }, []);
  useEffect(
    () => () => {
      if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    },
    [],
  );

  const handleBackToGrid = () => {
    if (isFullscreen) exitFullscreen();
    useVoiceStore.getState().setFullscreenStream(null);
  };

  const handleStopWatching = async () => {
    if (!displayUser) return;
    if (useVoiceStore.getState().callPeer && !connectedChannelId) {
      // P2P DM call: no community watch subscription — ungate natively.
      if (displayUser !== currentUsername) {
        await invoke("call_watch_stream", { watch: false }).catch(() => {});
      }
    } else {
      if (!connectedServerId || !connectedChannelId) return;
      if (displayUser !== currentUsername) {
        await invoke("stop_watching", {
          serverId: connectedServerId,
          channelId: connectedChannelId,
          targetUsername: displayUser,
        }).catch(() => {});
      }
    }
    // Self-preview unmount is renderer-internal: StreamVideoPlayer's
    // useEffect cleanup unsubscribes from subscribeLocalFrames when it
    // unmounts. No native side coordination needed.
    useVoiceStore.getState().removeWatching(displayUser);
    if (isFullscreen) exitFullscreen();
  };

  if (!displayUser || !stream) return null;

  const showVolume = !isOwnStream && stream.hasAudio;
  const overlayClass = `transition-opacity duration-150 ${
    overlayVisible ? "opacity-100" : "pointer-events-none opacity-0"
  }`;

  return (
    <div
      className={
        isFullscreen
          ? "fixed inset-0 z-50 flex flex-col bg-black"
          : // pb-21 keeps the filmstrip clear of VoicePanel's floating dock.
            "flex min-h-0 min-w-0 flex-1 flex-col gap-3 px-4 pb-21 pt-4"
      }
    >
      <div
        className={`relative flex min-h-0 min-w-0 flex-1 items-center justify-center overflow-hidden bg-black ${
          isFullscreen
            ? overlayVisible
              ? "cursor-default"
              : "cursor-none"
            : "rounded-lg border border-border"
        }`}
        onDoubleClick={() => void (isFullscreen ? exitFullscreen() : enterFullscreen())}
        onMouseMove={pokeOverlay}
        onMouseLeave={handleMouseLeave}
      >
        {/* The shared persistent stream player is reparented in here so it
            survives moving to/from the grid tile and the mini player. */}
        <div ref={pipSlotRef} className="h-full w-full" />

        {showStats && <StreamStatsOverlay username={displayUser} className="left-3 top-14" />}

        {/* top-left: what this is */}
        <div className={`pointer-events-none absolute left-3 top-3 flex gap-1.5 ${overlayClass}`}>
          <LivePill size="lg" />
          <span className={SCRIM_LG}>
            <UserAvatar username={displayUser} size={18} />
            {isOwnStream ? "Your screen" : `${displayName}'s screen`}
          </span>
          <QualityPill stream={stream} large />
        </div>

        {/* top-right: view controls */}
        <div
          className={`absolute right-3 top-3 flex items-center gap-1.5 ${overlayClass}`}
          onMouseEnter={holdOverlay}
          onDoubleClick={stopDouble}
        >
          {isFullscreen && <span className="mr-1.5 text-[11px] text-white/50">Esc to exit</span>}
          <OverlayButton title="Stream stats" active={showStats} onClick={() => setShowStats((v) => !v)}>
            <StatsIcon size={16} />
          </OverlayButton>
          {!isFullscreen && (
            <OverlayButton title="Back to the grid (Esc)" onClick={handleBackToGrid}>
              <GridIcon size={16} />
            </OverlayButton>
          )}
          <OverlayButton
            title={isFullscreen ? "Exit fullscreen" : "Fullscreen"}
            onClick={() => void (isFullscreen ? exitFullscreen() : enterFullscreen())}
          >
            {isFullscreen ? <CollapseIcon size={16} /> : <ExpandIcon size={16} />}
          </OverlayButton>
          <OverlayButton title="Stop watching" onClick={() => void handleStopWatching()}>
            <CloseIcon size={16} />
          </OverlayButton>
        </div>

        {/* bottom-left (panel): stream volume */}
        {!isFullscreen && showVolume && (
          <div className={`absolute bottom-3 left-3 ${overlayClass}`} onMouseEnter={holdOverlay} onDoubleClick={stopDouble}>
            <VolumeControl volume={streamVolume} onChange={handleVolumeChange} onToggleMute={toggleMute} />
          </div>
        )}

        {/* bottom (fullscreen): the dock, on a scrim — VoicePanel's own dock
            is hidden while the window is fullscreen. */}
        {isFullscreen && (
          <div
            className={`absolute inset-x-0 bottom-6 flex justify-center ${overlayClass}`}
            onMouseEnter={holdOverlay}
            onClick={(e) => e.stopPropagation()}
            onDoubleClick={stopDouble}
          >
            <div className="flex items-center gap-1.5 rounded-lg border border-white/10 bg-black/70 p-1.5 shadow-float">
              <FullscreenDockButton title={isMuted ? "Unmute" : "Mute"} danger={isMuted} onClick={handleMute}>
                {isMuted ? <MicOffIcon /> : <MicIcon />}
              </FullscreenDockButton>
              <FullscreenDockButton title={isDeafened ? "Undeafen" : "Deafen"} danger={isDeafened} onClick={handleDeafen}>
                {isDeafened ? <HeadphonesOffIcon /> : <HeadphonesIcon />}
              </FullscreenDockButton>
              {showVolume && (
                <>
                  <div className="mx-0.5 h-6 w-px bg-white/10" />
                  <VolumeControl volume={streamVolume} onChange={handleVolumeChange} onToggleMute={toggleMute} />
                </>
              )}
              <div className="mx-0.5 h-6 w-px bg-white/10" />
              <FullscreenDockButton title="Exit fullscreen" onClick={() => void exitFullscreen()}>
                <CollapseIcon />
              </FullscreenDockButton>
            </div>
          </div>
        )}
      </div>

      {!isFullscreen && <Filmstrip focused={displayUser} />}
    </div>
  );
}

function FullscreenDockButton({
  title,
  danger,
  onClick,
  children,
}: {
  title: string;
  danger?: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      title={title}
      aria-label={title}
      onClick={onClick}
      className={`flex h-10 w-10 items-center justify-center rounded-md ${
        danger ? "bg-error/20 text-error hover:bg-error/30" : "text-white/80 hover:bg-white/10 hover:text-white"
      }`}
    >
      {children}
    </button>
  );
}

/// The row under a focused stream: the other live streams first (watched ones
/// keep playing; a click focuses without stopping anything), then people as
/// mini tiles, capped to one row with the same "+N more" and hidden-speaker
/// swap as the grid. Subscribes to what it shows itself, so the focused
/// panel's overlay timers don't re-render it.
const Filmstrip = memo(function Filmstrip({ focused }: { focused: string }) {
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const watchingStreams = useVoiceStore((s) => s.watchingStreams);
  const participants = useVoiceStore((s) => s.participants);
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const ownUsername = useAuthStore((s) => s.username);
  const channelName = useChatStore((s) => {
    const list = s.activeServerId ? s.channelsByServer[s.activeServerId] ?? EMPTY_CHANNELS : EMPTY_CHANNELS;
    return list.find((ch) => ch.id === connectedChannelId)?.name ?? "Voice";
  });

  const ref = useRef<HTMLDivElement>(null);
  const { width } = useElementSize(ref);

  const others = activeStreams.filter((s) => s.ownerUsername !== focused);
  const rosterKey = participants.map((p) => p.username).join("\n");
  const roster = useMemo(() => (rosterKey ? rosterKey.split("\n") : []), [rosterKey]);
  const streamersKey = activeStreams.map((s) => s.ownerUsername).join("\n");
  const pinned = useMemo(() => {
    const set = new Set(streamersKey ? streamersKey.split("\n") : []);
    if (ownUsername) set.add(ownUsername);
    return set;
  }, [streamersKey, ownUsername]);
  const byName = useMemo(() => new Map(participants.map((p) => [p.username, p])), [participants]);

  const cap = width > 0 ? rowCapacity(width, STRIP_TILE_W, STRIP_GAP) : others.length + roster.length;
  const streamSlots = Math.min(others.length, cap);
  const peopleCap = cap - streamSlots;
  const slots = roster.length > peopleCap ? Math.max(0, peopleCap - 1) : roster.length;
  const overflow = slots < roster.length && peopleCap > 0;
  const visible = useVisibleParticipants(roster, slots, pinned);
  const hidden = useMemo(() => {
    if (!overflow) return [];
    const shown = new Set(visible);
    return roster.filter((u) => !shown.has(u));
  }, [overflow, visible, roster]);

  return (
    <div ref={ref} className="flex h-18 shrink-0 justify-center gap-2">
      {others.slice(0, streamSlots).map((s) => (
        <StreamTile
          key={s.streamId}
          stream={s}
          isWatching={watchingStreams.includes(s.ownerUsername)}
          // Never the persistent player here: that one is on stage. A
          // watched stream in the strip runs its own decoder.
          isPip={false}
          isOwnStream={s.ownerUsername === ownUsername}
          watchingOthers
          connectedServerId={connectedServerId}
          width={STRIP_TILE_W}
          mini
        />
      ))}
      {visible.map((u) => {
        const p = byName.get(u);
        return (
          <ParticipantTile
            key={u}
            username={u}
            isLocal={u === ownUsername}
            rosterMuted={p?.isMuted ?? false}
            rosterDeafened={p?.isDeafened ?? false}
            serverMuted={p?.isServerMuted}
            serverDeafened={p?.isServerDeafened}
            connectedServerId={connectedServerId}
            width={STRIP_TILE_W}
            mini
          />
        );
      })}
      {overflow && (
        <OverflowTile
          hidden={hidden}
          width={STRIP_TILE_W}
          mini
          connectedServerId={connectedServerId}
          channelName={channelName}
        />
      )}
    </div>
  );
});

// No props: VoicePanel re-renders (ping, participants, thumbnails) must not
// re-render this panel — it subscribes to what it needs itself.
export default memo(StreamViewPanel);
