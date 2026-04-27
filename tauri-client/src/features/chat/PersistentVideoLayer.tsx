import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useActiveVideoStore } from "../../stores/activeVideoStore";
import { useImageContextMenuStore } from "../../stores/imageContextMenuStore";
import { useUiStore } from "../../stores/uiStore";
import { toast } from "../../stores/toastStore";
import { pickSavePath } from "./filePicker";
import { saveSettings } from "../settings/saveSettings";
import { getCachedVideo, setVideoPoster, updateCachedVideoState } from "./tempVideoCache";

// Single mounted-once <video> element that survives Virtuoso row
// unmounts. The chat-side VideoPlayer becomes a placeholder that
// publishes its bounding rect via `useActiveVideoStore.hostElement`;
// we follow that rect via ResizeObserver + a RAF-throttled scroll
// listener and overlay the video on top. When the host unmounts
// (user scrolled past it), we park the video offscreen so playback
// continues uninterrupted, then snap back into position when a new
// host registers.

export default function PersistentVideoLayer() {
  const active = useActiveVideoStore((s) => s.active);
  const hostElement = useActiveVideoStore((s) => s.hostElement);

  if (!active) return null;
  return (
    <PersistentPlayer
      key={active.attachmentId}
      active={active}
      hostElement={hostElement}
    />
  );
}

interface ActivePlayerProps {
  active: NonNullable<ReturnType<typeof useActiveVideoStore.getState>["active"]>;
  hostElement: HTMLDivElement | null;
}

function PersistentPlayer({ active, hostElement }: ActivePlayerProps) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const capturedVideoRef = useRef<HTMLVideoElement | null>(null);

  useLayoutEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    capturedVideoRef.current = v;
    v.crossOrigin = "anonymous";
    // Seed volume + mute from the persisted level *before* setting
    // src so the element never plays a frame at the wrong loudness.
    const ui = useUiStore.getState();
    v.volume = ui.mediaVideoVolume;
    v.muted = ui.mediaVideoMuted;
    v.src = active.src;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const wrapperRef = useRef<HTMLDivElement | null>(null);

  const [pos, setPos] = useState<{ left: number; top: number; width: number; height: number } | null>(
    null,
  );
  // Clip rect of the host's nearest scrollable ancestor (the chat
  // messages list). Since the wrapper is `position: fixed` it escapes
  // that ancestor's overflow:hidden, so we manually clip-path the
  // wrapper so the video doesn't bleed over the input bar / channel
  // header when the host is partially or fully out of the visible
  // chat area.
  const [clipRect, setClipRect] = useState<{ top: number; bottom: number } | null>(null);

  useEffect(() => {
    if (!hostElement) {
      setPos(null);
      setClipRect(null);
      return;
    }
    // Find the nearest scrollable ancestor once per host-change.
    // Re-evaluating on every scroll tick would be wasteful and the
    // chat scroll container doesn't change for a given host.
    let scrollParent: HTMLElement | null = hostElement.parentElement;
    while (scrollParent) {
      const cs = window.getComputedStyle(scrollParent);
      if (/(auto|scroll|hidden)/.test(cs.overflowY) ||
          /(auto|scroll|hidden)/.test(cs.overflow)) {
        break;
      }
      scrollParent = scrollParent.parentElement;
    }

    const update = () => {
      const r = hostElement.getBoundingClientRect();
      setPos({ left: r.left, top: r.top, width: r.width, height: r.height });
      if (scrollParent) {
        const sr = scrollParent.getBoundingClientRect();
        setClipRect({ top: sr.top, bottom: sr.bottom });
      } else {
        setClipRect(null);
      }
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(hostElement);
    if (scrollParent) ro.observe(scrollParent);
    let frame = 0;
    const onScrollOrResize = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(update);
    };
    window.addEventListener("scroll", onScrollOrResize, true);
    window.addEventListener("resize", onScrollOrResize);
    return () => {
      ro.disconnect();
      cancelAnimationFrame(frame);
      window.removeEventListener("scroll", onScrollOrResize, true);
      window.removeEventListener("resize", onScrollOrResize);
    };
  }, [hostElement]);

  // ---- Video state mirrored from element events ----
  const [playing, setPlaying] = useState(false);
  const [time, setTime] = useState(0);
  const [duration, setDuration] = useState(0);
  // Volume + mute live in uiStore so they persist across restarts.
  // We seed the element from these on mount and mirror element-side
  // changes back via the volumechange listener below.
  const muted = useUiStore((s) => s.mediaVideoMuted);
  const volume = useUiStore((s) => s.mediaVideoVolume);
  const [scrubbing, setScrubbing] = useState(false);
  const [hover, setHover] = useState(false);
  const [fullscreen, setFullscreenState] = useState(false);
  const ownsWindowFullscreen = useRef(false);
  const [idle, setIdle] = useState(false);
  const idleTimerRef = useRef<number | null>(null);

  const setFullscreen = async (on: boolean) => {
    setFullscreenState(on);
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      await getCurrentWindow().setFullscreen(on);
      ownsWindowFullscreen.current = on;
    } catch {
      // Compositor refused; CSS fullscreen still applies.
    }
  };

  useEffect(() => {
    return () => {
      if (ownsWindowFullscreen.current) {
        import("@tauri-apps/api/window")
          .then(({ getCurrentWindow }) => getCurrentWindow().setFullscreen(false))
          .catch(() => {});
      }
    };
  }, []);

  useEffect(() => {
    return () => {
      const v = capturedVideoRef.current;
      if (!v || !v.videoWidth || !v.videoHeight) return;
      try {
        const canvas = document.createElement("canvas");
        canvas.width = v.videoWidth;
        canvas.height = v.videoHeight;
        const ctx = canvas.getContext("2d");
        if (!ctx) return;
        ctx.drawImage(v, 0, 0);
        canvas.toBlob(
          (blob) => {
            if (!blob) return;
            const url = URL.createObjectURL(blob);
            setVideoPoster(active.channelId, active.attachmentId, url);
          },
          "image/jpeg",
          0.7,
        );
      } catch {
        // Tainted canvas (CORS) or other capture failure
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const resetIdleTimer = () => {
    setIdle(false);
    if (idleTimerRef.current !== null) {
      window.clearTimeout(idleTimerRef.current);
    }
    idleTimerRef.current = window.setTimeout(() => setIdle(true), 3000);
  };
  useEffect(() => {
    return () => {
      if (idleTimerRef.current !== null) window.clearTimeout(idleTimerRef.current);
    };
  }, []);

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const cached = getCachedVideo(active.channelId, active.attachmentId);
    const seekIfNeeded = () => {
      if (cached && cached.lastTime > 0 && cached.lastTime < v.duration) {
        v.currentTime = cached.lastTime;
      }
      v.play().catch(() => {});
    };
    if (v.readyState >= 1) seekIfNeeded();
    else v.addEventListener("loadedmetadata", seekIfNeeded, { once: true });
    return () => v.removeEventListener("loadedmetadata", seekIfNeeded);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const v = videoRef.current;
    if (!v) return;
    const onPlay = () => setPlaying(true);
    const onPause = () => setPlaying(false);
    const onTime = () => {
      setTime(v.currentTime);
      updateCachedVideoState(
        active.channelId,
        active.attachmentId,
        v.currentTime,
        !v.paused,
      );
    };
    const onDur = () => setDuration(v.duration || 0);
    const onVolume = () => {
      // Mirror element → uiStore + persist. Skip when nothing changed
      // so the seeding-time volumechange doesn't trigger a redundant
      // settings save on every fresh mount.
      const ui = useUiStore.getState();
      if (v.volume !== ui.mediaVideoVolume || v.muted !== ui.mediaVideoMuted) {
        ui.setMediaVideoVolume(v.volume);
        ui.setMediaVideoMuted(v.muted);
        saveSettings();
      }
    };
    v.addEventListener("play", onPlay);
    v.addEventListener("pause", onPause);
    v.addEventListener("timeupdate", onTime);
    v.addEventListener("loadedmetadata", onDur);
    v.addEventListener("durationchange", onDur);
    v.addEventListener("volumechange", onVolume);
    return () => {
      v.removeEventListener("play", onPlay);
      v.removeEventListener("pause", onPause);
      v.removeEventListener("timeupdate", onTime);
      v.removeEventListener("loadedmetadata", onDur);
      v.removeEventListener("durationchange", onDur);
      v.removeEventListener("volumechange", onVolume);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const controlsVisible = !playing || scrubbing || (hover && !idle);
  const cursorHidden = playing && hover && idle && !scrubbing;
  useEffect(() => {
    if (playing) resetIdleTimer();
    else {
      setIdle(false);
      if (idleTimerRef.current !== null) {
        window.clearTimeout(idleTimerRef.current);
        idleTimerRef.current = null;
      }
    }
  }, [playing]);

  // ---- Interaction handlers ----
  const togglePlay = () => {
    const v = videoRef.current;
    if (!v) return;
    if (v.paused) v.play().catch(() => {});
    else v.pause();
  };

  // Single-click toggles play after a short delay so that a double-
  // click (which fires after both single-clicks) can cancel it and
  // toggle fullscreen instead. Mirrors YouTube's behaviour. The 220 ms
  // delay sits just above a typical OS dblclick interval.
  const clickTimeoutRef = useRef<number | null>(null);
  const handleSingleClick = () => {
    if (clickTimeoutRef.current !== null) return;
    clickTimeoutRef.current = window.setTimeout(() => {
      clickTimeoutRef.current = null;
      togglePlay();
    }, 220);
  };
  const handleDoubleClick = () => {
    if (clickTimeoutRef.current !== null) {
      window.clearTimeout(clickTimeoutRef.current);
      clickTimeoutRef.current = null;
    }
    setFullscreen(!fullscreen);
  };
  // Cleanup the pending single-click on unmount so it doesn't fire
  // against a torn-down videoRef.
  useEffect(() => {
    return () => {
      if (clickTimeoutRef.current !== null) {
        window.clearTimeout(clickTimeoutRef.current);
        clickTimeoutRef.current = null;
      }
    };
  }, []);

  const handleDownload = async () => {
    let dest: string | null = null;
    try {
      dest = await pickSavePath({
        title: "Save video",
        defaultName: active.filename || "video",
      });
    } catch (err) {
      toast.error("Save dialog failed", String(err));
      return;
    }
    if (!dest) return;
    try {
      await invoke("download_attachment", {
        req: {
          serverId: active.serverId,
          attachmentId: active.attachmentId,
          destinationPath: dest,
        },
      });
      toast.success("Video saved", active.filename);
    } catch (err) {
      toast.error("Save failed", String(err));
    }
  };

  const handleContextMenu = (e: React.MouseEvent) => {
    // Suppress the context menu on the controls bar so right-clicking
    // a button doesn't pop up a "Save video" menu over it. Anything
    // outside the bar (the video itself) gets the menu.
    const target = e.target as HTMLElement | null;
    if (target?.closest("[data-controls-bar]")) return;
    e.preventDefault();
    useImageContextMenuStore.getState().show({
      x: e.clientX,
      y: e.clientY,
      serverId: active.serverId,
      attachmentId: active.attachmentId,
      filename: active.filename,
      kind: "video",
    });
  };
  const toggleMute = () => {
    const v = videoRef.current;
    if (!v) return;
    v.muted = !v.muted;
  };
  const seekTo = (t: number) => {
    const v = videoRef.current;
    if (!v) return;
    v.currentTime = Math.max(0, Math.min(duration, t));
    setTime(v.currentTime);
  };
  const handleScrubMouse = (clientX: number, trackEl: HTMLElement) => {
    const r = trackEl.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (clientX - r.left) / r.width));
    seekTo(ratio * duration);
  };
  const onScrubDown = (e: React.MouseEvent<HTMLDivElement>) => {
    setScrubbing(true);
    handleScrubMouse(e.clientX, e.currentTarget);
    const track = e.currentTarget;
    const onMove = (ev: MouseEvent) => handleScrubMouse(ev.clientX, track);
    const onUp = () => {
      setScrubbing(false);
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };
  const handleVolumeMouse = (clientX: number, trackEl: HTMLElement) => {
    const v = videoRef.current;
    if (!v) return;
    const r = trackEl.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (clientX - r.left) / r.width));
    v.volume = ratio;
    if (ratio > 0 && v.muted) v.muted = false;
    if (ratio === 0) v.muted = true;
  };
  const onVolumeDown = (e: React.MouseEvent<HTMLDivElement>) => {
    handleVolumeMouse(e.clientX, e.currentTarget);
    const track = e.currentTarget;
    const onMove = (ev: MouseEvent) => handleVolumeMouse(ev.clientX, track);
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };
  // Scroll-wheel volume — 5% step per notch, same as the audio player.
  const onVolumeWheel = (e: React.WheelEvent) => {
    const v = videoRef.current;
    if (!v) return;
    e.preventDefault();
    const step = e.deltaY < 0 ? 0.05 : -0.05;
    const next = Math.max(0, Math.min(1, (v.muted ? 0 : v.volume) + step));
    v.volume = next;
    if (next > 0 && v.muted) v.muted = false;
    if (next === 0) v.muted = true;
  };

  const seekRateRef = useRef(0);
  const handleKey = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement | null;
    const tag = target?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || target?.isContentEditable) return;
    const v = videoRef.current;
    if (!v) return;
    if (e.key === " " || e.code === "Space") {
      e.preventDefault();
      if (v.paused) v.play().catch(() => {});
      else v.pause();
    } else if (e.key === "ArrowLeft" || e.key === "ArrowRight") {
      e.preventDefault();
      if (e.repeat) {
        const now = performance.now();
        if (now - seekRateRef.current < 80) return;
        seekRateRef.current = now;
      }
      const delta = e.key === "ArrowRight" ? 5 : -5;
      v.currentTime = Math.max(0, Math.min(v.duration || 0, v.currentTime + delta));
      setTime(v.currentTime);
    } else if (e.key === "f" || e.key === "F") {
      e.preventDefault();
      setFullscreen(!fullscreen);
    } else if (e.key === "Escape" && fullscreen) {
      setFullscreen(false);
    }
  };

  const fmt = (s: number) => {
    if (!isFinite(s) || s < 0) return "0:00";
    const m = Math.floor(s / 60);
    const sec = Math.floor(s % 60);
    return `${m}:${sec.toString().padStart(2, "0")}`;
  };
  const progress = duration > 0 ? (time / duration) * 100 : 0;

  // Translate the chat-area clip rect into an element-local inset so
  // the wrapper hides anything that's scrolled above the chat top or
  // below its bottom (input bar, channel header). Skipped in fullscreen
  // since the wrapper takes over the viewport.
  let clipPath: string | undefined;
  if (!fullscreen && pos && clipRect) {
    const topInset = Math.max(0, clipRect.top - pos.top);
    const bottomInset = Math.max(0, pos.top + pos.height - clipRect.bottom);
    if (topInset > 0 || bottomInset > 0) {
      clipPath = `inset(${topInset}px 0 ${bottomInset}px 0)`;
    }
  }

  const wrapperStyle: React.CSSProperties = fullscreen
    ? { position: "fixed", inset: 0, zIndex: 100 }
    : pos
      ? {
          position: "fixed",
          left: pos.left,
          top: pos.top,
          width: pos.width,
          height: pos.height,
          zIndex: 30,
          clipPath,
        }
      : {
          position: "fixed",
          left: -10000,
          top: -10000,
          width: 1,
          height: 1,
          zIndex: 30,
          visibility: "hidden",
          pointerEvents: "none",
        };

  return (
    <div
      ref={wrapperRef}
      className={`flex items-center justify-center overflow-hidden bg-bg-darkest outline-none ${
        fullscreen ? "" : "rounded-xl border border-border"
      } ${cursorHidden ? "cursor-none" : ""}`}
      style={wrapperStyle}
      tabIndex={-1}
      data-video-player
      onMouseDownCapture={() => wrapperRef.current?.focus()}
      onMouseEnter={() => {
        setHover(true);
        if (playing) resetIdleTimer();
      }}
      onMouseLeave={() => setHover(false)}
      onMouseMove={() => {
        setHover(true);
        if (playing) resetIdleTimer();
      }}
      onKeyDown={handleKey}
      onDoubleClick={handleDoubleClick}
      onContextMenu={handleContextMenu}
    >
      <video
        ref={videoRef}
        autoPlay
        onClick={handleSingleClick}
        className={`h-full w-full ${cursorHidden ? "cursor-none" : "cursor-pointer"} bg-bg-darkest object-contain`}
      />

      {/* Top-right download button — fullscreen only. Inherits the
          same visibility rules as the bottom controls bar so it fades
          out with the cursor on idle. */}
      {fullscreen && (
        <div
          data-controls-bar
          className={`pointer-events-none absolute right-4 top-4 transition-all duration-300 ease-out ${
            controlsVisible ? "translate-y-0 opacity-100" : "-translate-y-2 opacity-0"
          }`}
        >
          <button
            onClick={(e) => {
              e.stopPropagation();
              handleDownload();
            }}
            title="Download"
            className="pointer-events-auto flex h-10 w-10 items-center justify-center rounded-md bg-bg-darkest/85 text-white/85 backdrop-blur-md transition-colors hover:bg-bg-darkest hover:text-white"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
              <polyline points="7 10 12 15 17 10" />
              <line x1="12" y1="15" x2="12" y2="3" />
            </svg>
          </button>
        </div>
      )}

      {/* Controls overlay */}
      <div
        data-controls-bar
        className={`pointer-events-none absolute inset-x-0 bottom-0 transition-all duration-300 ease-out ${
          controlsVisible ? "translate-y-0 opacity-100" : "translate-y-2 opacity-0"
        }`}
      >
        <div className="bg-gradient-to-t from-black/85 via-black/50 to-transparent px-3.5 pb-2.5 pt-10">
          {/* Progress bar */}
          <div
            className="pointer-events-auto group relative mb-2.5 flex h-3 cursor-pointer items-center"
            onMouseDown={onScrubDown}
          >
            <div className="pointer-events-none absolute inset-x-0 h-[4px] rounded-full bg-white/15" />
            <div
              className="pointer-events-none absolute h-[4px] rounded-full bg-accent"
              style={{ width: `${progress}%` }}
            />
            <div
              className="pointer-events-none absolute h-3 w-3 -translate-x-1/2 rounded-full border-2 border-accent bg-bg-darkest opacity-0 shadow-[0_0_6px_rgba(56,143,255,0.3)] transition-opacity group-hover:opacity-100"
              style={{ left: `${progress}%` }}
            />
          </div>

          {/* Bottom controls row */}
          <div className="pointer-events-auto flex items-center gap-2">
            <PlayerIconButton title={playing ? "Pause" : "Play"} onClick={togglePlay}>
              {playing ? (
                <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                  <rect x="6" y="5" width="4" height="14" rx="1" />
                  <rect x="14" y="5" width="4" height="14" rx="1" />
                </svg>
              ) : (
                <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor">
                  <path d="M8 5v14l11-7z" />
                </svg>
              )}
            </PlayerIconButton>
            <span className="select-none text-[11px] tabular-nums text-text-secondary">
              {fmt(time)} / {fmt(duration)}
            </span>
            <span className="ml-auto truncate text-[11px] text-text-muted" title={active.filename}>
              {active.filename}
            </span>
            <div className="flex items-center gap-1">
              <PlayerIconButton title={muted ? "Unmute" : "Mute"} onClick={toggleMute}>
                {muted ? (
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
                    <line x1="23" y1="9" x2="17" y2="15" />
                    <line x1="17" y1="9" x2="23" y2="15" />
                  </svg>
                ) : (
                  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
                    <path d="M19.07 4.93a10 10 0 010 14.14M15.54 8.46a5 5 0 010 7.07" />
                  </svg>
                )}
              </PlayerIconButton>
              <div
                className="group relative flex h-3 w-20 cursor-pointer items-center"
                onMouseDown={onVolumeDown}
                onWheel={onVolumeWheel}
                title="Volume"
              >
                <div className="pointer-events-none absolute inset-x-0 h-[4px] rounded-full bg-white/15" />
                <div
                  className="pointer-events-none absolute h-[4px] rounded-full bg-accent"
                  style={{ width: `${(muted ? 0 : volume) * 100}%` }}
                />
                <div
                  className="pointer-events-none absolute h-3 w-3 -translate-x-1/2 rounded-full border-2 border-accent bg-bg-darkest opacity-0 shadow-[0_0_6px_rgba(56,143,255,0.3)] transition-opacity group-hover:opacity-100"
                  style={{ left: `${(muted ? 0 : volume) * 100}%` }}
                />
              </div>
            </div>
            <PlayerIconButton
              title={fullscreen ? "Exit fullscreen" : "Fullscreen"}
              onClick={() => setFullscreen(!fullscreen)}
            >
              {fullscreen ? (
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M8 3v3a2 2 0 0 1-2 2H3" />
                  <path d="M21 8h-3a2 2 0 0 1-2-2V3" />
                  <path d="M3 16h3a2 2 0 0 1 2 2v3" />
                  <path d="M16 21v-3a2 2 0 0 1 2-2h3" />
                </svg>
              ) : (
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M3 8V5a2 2 0 0 1 2-2h3" />
                  <path d="M21 8V5a2 2 0 0 0-2-2h-3" />
                  <path d="M3 16v3a2 2 0 0 0 2 2h3" />
                  <path d="M21 16v3a2 2 0 0 1-2 2h-3" />
                </svg>
              )}
            </PlayerIconButton>
          </div>
        </div>
      </div>
    </div>
  );
}

function PlayerIconButton({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      title={title}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className="flex h-8 w-8 cursor-pointer items-center justify-center rounded-md text-white/80 transition-colors hover:bg-surface-active hover:text-white"
    >
      {children}
    </button>
  );
}
