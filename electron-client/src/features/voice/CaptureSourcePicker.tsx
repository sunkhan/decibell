import { useState, useEffect, useCallback, useRef } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";
import { useCodecSettingsStore } from "../../stores/codecSettingsStore";
import { VideoCodec, type CaptureSource } from "../../types";
import { playSound } from "../../utils/sounds";
import {
  activeStreamCapture,
  startActiveStream,
  stopActiveStream,
} from "./streaming/StreamCapture";
import { isNativeEncodeActive } from "../../utils/encoderProbe";
import { announceCallStreamStart, announceCallStreamStop } from "../call/callActions";
import SegmentedControl from "../../components/SegmentedControl";
import Switch from "../../components/Switch";
import StreamAudioAppPicker from "./StreamAudioAppPicker";
import { canPickStreamAudioApps } from "./streamAudioFilter";

interface Props {
  /// Community voice channel to announce in. Both absent inside a P2P DM
  /// call — the stream is announced to the peer over central instead.
  serverId?: string;
  channelId?: string;
  onClose: () => void;
}

/// Platforms where we render our own tabbed source picker. Linux is
/// excluded — getDisplayMedia goes through xdg-desktop-portal there
/// and the portal dialog is the picker. macOS uses Electron's
/// `useSystemPicker: true` to get the native ScreenCaptureKit dialog.
const NEEDS_CUSTOM_PICKER =
  typeof window !== "undefined" && window.decibell?.platform === "win32";

export default function CaptureSourcePicker({
  serverId,
  channelId,
  onClose,
}: Props) {
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [visible, setVisible] = useState(false);
  const [closing, setClosing] = useState(false);
  const [pickedSourceId, setPickedSourceId] = useState<string | null>(null);

  const streamSettings = useVoiceStore((s) => s.streamSettings);
  const setStreamSettings = useVoiceStore((s) => s.setStreamSettings);

  useEffect(() => {
    requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
  }, []);

  const handleClose = useCallback(() => {
    if (closing) return;
    setClosing(true);
    setVisible(false);
  }, [closing]);

  const handleTransitionEnd = useCallback(() => {
    if (!visible && closing) onClose();
  }, [visible, closing, onClose]);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") handleClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [handleClose]);

  // PR8: Chromium's `getDisplayMedia` triggers the OS-native screen-share
  // dialog when StreamCapture.start() runs, so the picker no longer needs
  // its own source list. Settings + Go Live only.
  //
  // For "source", the actual dimensions are read off the negotiated
  // track inside StreamCapture (useNativeSize flag below). The numbers
  // returned here are only used for the StartStreamReq packet to the
  // server, the bitrate-preset table, and the encoder's pre-flight
  // bitrate check. Use a generous 1440p stand-in so the bitrate ceiling
  // covers most native-resolution sources without bottlenecking; the
  // encoder reconfigures with the real numbers once Chromium negotiates.
  const resolveDimensions = (): { width: number; height: number } => {
    switch (streamSettings.resolution) {
      case "720p":
        return { width: 1280, height: 720 };
      case "source":
        return { width: 2560, height: 1440 };
      default:
        return { width: 1920, height: 1080 };
    }
  };

  const handleGoLive = async () => {
    if (NEEDS_CUSTOM_PICKER && !pickedSourceId) {
      setError("Pick a screen or window to share first.");
      return;
    }
    setStarting(true);
    setError(null);
    try {
      const dims = resolveDimensions();
      const codec =
        streamSettings.enforcedCodec === VideoCodec.UNKNOWN
          ? VideoCodec.H264_HW
          : streamSettings.enforcedCodec;

      // Renderer side first: prompt for capture source, peek the
      // first frame, configure the encoder. start() returns the
      // *actual* dimensions Chromium negotiated — those are what we
      // announce to the server, so the resolution badge and presence
      // payload reflect reality even when the user picked "Source"
      // and we couldn't predict it. If the user cancels the OS
      // dialog, getDisplayMedia rejects and we never bother native.
      const stream = await startActiveStream({
        codec,
        width: dims.width,
        height: dims.height,
        fps: streamSettings.fps,
        bitrateKbps: streamSettings.videoBitrateKbps,
        shareAudio: streamSettings.shareAudio,
        audioBitrateKbps: streamSettings.audioBitrateKbps,
        audioMode: streamSettings.audioMode,
        audioApps: streamSettings.audioApps,
        includeCursor: streamSettings.includeCursor,
        serverId,
        channelId,
        useNativeSize: streamSettings.resolution === "source",
        sourceId: pickedSourceId ?? undefined,
        onCaptureEnded: () => {
          useVoiceStore.getState().setIsStreaming(false);
          invoke("stop_screen_share", { serverId, channelId }).catch(() => {});
          announceCallStreamStop();
          playSound("stream_stop");
        },
      });
      let actualDims: { width: number; height: number };
      try {
        actualDims = await stream.start();
      } catch (e) {
        await stopActiveStream();
        throw e;
      }
      // What the session really encodes: a native→WebCodecs fallback
      // downgrades HEVC/AV1 to H.264. Announcing the requested codec made
      // the server enforce HEVC on watchers of an H.264 stream. An
      // enforcement the stream no longer satisfies is dropped.
      const activeCodec = stream.activeCodec;
      const enforcedCodec =
        streamSettings.enforcedCodec && streamSettings.enforcedCodec === activeCodec
          ? streamSettings.enforcedCodec
          : 0;
      // The session can end while we await (encoder error, native failure
      // event, a stop from elsewhere). Don't resurrect it as "streaming".
      const superseded = () => activeStreamCapture() !== stream;
      if (superseded()) throw new Error("Stream ended while starting");

      // Native signaling: register the stream with the truthful
      // dimensions. On Linux/macOS this is where StartStreamReq
      // goes out + VideoEngine is created (the renderer's WebCodecs
      // encoder, already running via stream.start() above, will start
      // pumping send_video_frame in a few ms).
      //
      // On the native path (Windows always; Linux when a HW encoder was
      // probed) stream.start() ALREADY invoked start_screen_share to spin
      // up native capture + encode AND send the StartStreamReq — calling
      // it again bounces off the "Already sharing screen" guard. Only the
      // renderer-WebCodecs path (Linux without native, macOS) needs this
      // separate call to create the VideoEngine sender + announce.
      if (!isNativeEncodeActive()) {
        try {
          await invoke("start_screen_share", {
            serverId,
            channelId,
            fps: streamSettings.fps,
            width: actualDims.width,
            height: actualDims.height,
            videoBitrateKbps: streamSettings.videoBitrateKbps,
            shareAudio: streamSettings.shareAudio,
            audioBitrateKbps: streamSettings.audioBitrateKbps,
            audioMode: streamSettings.audioMode,
            audioApps: streamSettings.audioApps,
            initialCodec: activeCodec,
            enforcedCodec,
            // Explicit false: the renderer owns capture + encode on this
            // branch. Windows honours this now (WebCodecs fallback) —
            // omitting it defaults to the native pipeline there.
            nativeEncode: false,
            includeCursor: streamSettings.includeCursor,
          });
        } catch (e) {
          await stopActiveStream();
          throw e;
        }
      }

      if (superseded()) {
        // Its stop may have run before the start_screen_share above
        // created the WebCodecs-path VideoEngine — tear that down too.
        if (!isNativeEncodeActive()) {
          await invoke("stop_screen_share", { serverId, channelId }).catch(() => {});
        }
        throw new Error("Stream ended while starting");
      }
      useVoiceStore.getState().setIsStreaming(true);
      // P2P DM call: no community presence broadcast — tell the peer
      // directly (no-op outside a call).
      announceCallStreamStart({
        codec: activeCodec,
        width: actualDims.width,
        height: actualDims.height,
        fps: streamSettings.fps,
        hasAudio: streamSettings.shareAudio,
      });
      playSound("stream_start");
      handleClose();
    } catch (e) {
      setError(String(e));
      setStarting(false);
    }
  };

  // Windows gets the source grid too: side by side with the settings on a
  // wide window (one fixed-height dialog, each column scrolls on its own),
  // stacked below `lg`. Either way the header and footer are pinned — the
  // body scrolls, never the Go Live button.
  const split = NEEDS_CUSTOM_PICKER;

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center p-6 transition-colors duration-300"
      style={{
        backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)",
      }}
      onClick={(e) => e.target === e.currentTarget && handleClose()}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby="go-live-title"
        className={`flex max-h-full w-full max-w-[560px] flex-col overflow-hidden rounded-xl border border-border bg-bg-dark shadow-modal transition-all duration-300 ${
          split ? "lg:h-full lg:max-h-[900px] lg:max-w-[1040px]" : ""
        }`}
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
      >
        <div className="flex shrink-0 items-center justify-between border-b border-border-divider px-5 py-4">
          <h2
            id="go-live-title"
            className="font-display text-[16px] font-semibold text-text-primary"
          >
            Go live
          </h2>
          <button
            onClick={handleClose}
            aria-label="Close"
            className="flex h-7 w-7 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        <div
          className={`flex min-h-0 flex-1 flex-col overflow-y-auto ${
            split ? "lg:flex-row lg:overflow-hidden" : ""
          }`}
        >
          {NEEDS_CUSTOM_PICKER ? (
            <SourceGrid
              pickedSourceId={pickedSourceId}
              onPick={setPickedSourceId}
            />
          ) : (
            <div className="mx-5 mt-5 mb-4 flex shrink-0 items-center gap-3 rounded-md border border-border-divider bg-bg-light px-4 py-3">
              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-accent-soft">
                <svg
                  width="18"
                  height="18"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  className="text-accent-bright"
                >
                  <rect x="2" y="3" width="20" height="14" rx="2" />
                  <line x1="8" y1="21" x2="16" y2="21" />
                  <line x1="12" y1="17" x2="12" y2="21" />
                </svg>
              </div>
              <div className="min-w-0">
                <p className="text-[13px] font-medium text-text-primary">
                  Screen or window selection
                </p>
                <p className="mt-0.5 text-[12px] leading-[1.55] text-text-muted">
                  A system dialog will appear after you click Go Live to choose what to share.
                </p>
              </div>
            </div>
          )}

          <div
            className={`mx-5 mb-5 flex shrink-0 flex-col gap-5 rounded-md border border-border-divider bg-bg-light p-4 ${
              split
                ? "lg:m-0 lg:w-[480px] lg:overflow-y-auto lg:rounded-none lg:border-0 lg:border-l lg:p-5"
                : ""
            }`}
          >
            <section className="flex flex-col gap-3">
              <h3 className={GROUP_HEADING}>Video</h3>
              <div className="flex gap-3">
                <div className="min-w-0 flex-1">
                  <label className={FIELD_LABEL}>Resolution</label>
                  <SegmentedControl
                    options={[
                      { value: "source" as const, label: "Source" },
                      { value: "1080p" as const, label: "1080p" },
                      { value: "720p" as const, label: "720p" },
                    ]}
                    value={streamSettings.resolution}
                    onChange={(v) => {
                      setStreamSettings({ resolution: v });
                      if (streamSettings.quality !== "custom") {
                        const isHighRes = v === "source";
                        const presets = {
                          low: isHighRes ? 6000 : 3000,
                          medium: isHighRes ? 12000 : 6000,
                          high: isHighRes ? 20000 : 10000,
                        };
                        setStreamSettings({
                          videoBitrateKbps: presets[streamSettings.quality],
                        });
                      }
                    }}
                  />
                </div>
                <div className="min-w-0 flex-1">
                  <label className={FIELD_LABEL}>Frame rate</label>
                  <SegmentedControl
                    options={[
                      { value: 120 as const, label: "120" },
                      { value: 60 as const, label: "60" },
                      { value: 30 as const, label: "30" },
                      { value: 15 as const, label: "15" },
                    ]}
                    value={streamSettings.fps}
                    onChange={(v) => setStreamSettings({ fps: v })}
                  />
                </div>
              </div>

              <div>
                <label className={FIELD_LABEL}>Video quality</label>
                <div className="flex rounded-md bg-bg-darkest p-[3px]">
                  {(() => {
                    const isHighRes = streamSettings.resolution === "source";
                    return [
                      {
                        key: "low" as const,
                        label: "Low",
                        sub: isHighRes ? "6 Mbps" : "3 Mbps",
                        bitrate: isHighRes ? 6000 : 3000,
                      },
                      {
                        key: "medium" as const,
                        label: "Medium",
                        sub: isHighRes ? "12 Mbps" : "6 Mbps",
                        bitrate: isHighRes ? 12000 : 6000,
                      },
                      {
                        key: "high" as const,
                        label: "High",
                        sub: isHighRes ? "20 Mbps" : "10 Mbps",
                        bitrate: isHighRes ? 20000 : 10000,
                      },
                      { key: "custom" as const, label: "Custom", sub: null, bitrate: null },
                    ];
                  })().map((opt) => (
                    <button
                      key={opt.key}
                      onClick={() => {
                        if (opt.bitrate !== null) {
                          setStreamSettings({
                            quality: opt.key,
                            videoBitrateKbps: opt.bitrate,
                          });
                        } else {
                          setStreamSettings({ quality: "custom" });
                        }
                      }}
                      className={`flex flex-1 flex-col items-center rounded-sm px-2 py-[7px] transition-all ${
                        streamSettings.quality === opt.key
                          ? "bg-accent-mid text-accent-bright shadow-[0_0_6px_color-mix(in_srgb,var(--color-accent)_10%,transparent)]"
                          : "text-text-muted hover:text-text-secondary"
                      }`}
                    >
                      <span className="text-[11px] font-semibold">{opt.label}</span>
                      {opt.sub && (
                        <span
                          className={`text-[10px] ${
                            streamSettings.quality === opt.key
                              ? "text-accent/60"
                              : "text-text-muted"
                          }`}
                        >
                          {opt.sub}
                        </span>
                      )}
                    </button>
                  ))}
                </div>

                {streamSettings.quality === "custom" && (
                  <div className="mt-2.5 flex items-center gap-3 px-1">
                    <input
                      type="range"
                      min={1000}
                      max={30000}
                      step={500}
                      value={streamSettings.videoBitrateKbps}
                      onChange={(e) =>
                        setStreamSettings({
                          videoBitrateKbps: Number(e.target.value),
                        })
                      }
                      className="h-[6px] flex-1 cursor-pointer appearance-none rounded-full bg-bg-lighter accent-accent [&::-webkit-slider-thumb]:h-[16px] [&::-webkit-slider-thumb]:w-[16px] [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-mid [&::-webkit-slider-thumb]:shadow-[0_0_6px_color-mix(in_srgb,var(--color-accent)_30%,transparent)]"
                    />
                    <span className="w-[60px] shrink-0 whitespace-nowrap text-right text-[11px] font-medium tabular-nums text-text-secondary">
                      {streamSettings.videoBitrateKbps >= 1000
                        ? `${(streamSettings.videoBitrateKbps / 1000).toFixed(streamSettings.videoBitrateKbps % 1000 === 0 ? 0 : 1)} Mbps`
                        : `${streamSettings.videoBitrateKbps} kbps`}
                    </span>
                  </div>
                )}
              </div>

              <CodecPicker />

              <label className="flex cursor-pointer items-center justify-between gap-3 text-[13px] text-text-secondary">
                Show cursor
                <Switch
                  checked={streamSettings.includeCursor}
                  onToggle={() =>
                    setStreamSettings({
                      includeCursor: !streamSettings.includeCursor,
                    })
                  }
                />
              </label>
            </section>

            <section className="flex flex-col gap-3 border-t border-border-divider pt-5">
              <label className="flex cursor-pointer items-center justify-between gap-3">
                <span className={GROUP_HEADING}>Audio</span>
                <Switch
                  checked={streamSettings.shareAudio}
                  label="Share audio"
                  onToggle={() =>
                    setStreamSettings({ shareAudio: !streamSettings.shareAudio })
                  }
                />
              </label>

              {!streamSettings.shareAudio ? (
                <p className="text-[12px] text-text-muted">
                  Audio is off — viewers get video only.
                </p>
              ) : (
                <>
                  <div>
                    <label className={FIELD_LABEL}>Audio bitrate</label>
                    <SegmentedControl
                      options={[
                        { value: 128 as const, label: "128 kbps" },
                        { value: 192 as const, label: "192 kbps" },
                      ]}
                      value={streamSettings.audioBitrateKbps}
                      onChange={(v) => setStreamSettings({ audioBitrateKbps: v })}
                    />
                  </div>

                  {/* Per-app audio exists only on the native capture path; the
                      picker hides itself on macOS / the WebCodecs fallback. */}
                  {canPickStreamAudioApps() && (
                    <div>
                      <label className={FIELD_LABEL}>Audio from</label>
                      <StreamAudioAppPicker sourceId={pickedSourceId ?? undefined} />
                    </div>
                  )}
                </>
              )}
            </section>
          </div>
        </div>

        <div className="shrink-0 border-t border-border-divider px-5 py-3">
          {error && (
            <p className="mb-2 line-clamp-3 break-words text-[12px] text-error" title={error}>
              {error}
            </p>
          )}
          <div className="flex items-center gap-2">
            <p className="min-w-0 flex-1 truncate text-[12px] text-text-muted">
              {NEEDS_CUSTOM_PICKER && !pickedSourceId
                ? "Pick a screen or window to share."
                : null}
            </p>
            <button
              onClick={handleClose}
              className="rounded-sm border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
            >
              Cancel
            </button>
            <button
              onClick={handleGoLive}
              disabled={starting || (NEEDS_CUSTOM_PICKER && !pickedSourceId)}
              className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent transition-colors hover:bg-accent-hover disabled:opacity-50 disabled:hover:bg-accent"
            >
              {starting ? "Starting..." : "Go Live"}
            </button>
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}

const FIELD_LABEL =
  "mb-2 block text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted";
const GROUP_HEADING = "font-display text-[13px] font-semibold text-text-primary";

/// Tabbed grid of screens + windows, populated from Chromium's
/// desktopCapturer via the preload bridge. Thumbnails are JPEG data URLs
/// (Chromium decodes them on assignment to <img>; no extra trip through
/// canvas). Re-polled at REFRESH_MS so the previews don't go stale while
/// the modal is open — Chromium re-snapshots the surfaces server-side,
/// which is the same path the in-browser screen-share dialog uses.
///
/// Each poll is synchronous image-encoding work in the main process (it
/// stalls every window), so it is kept small: a slow cadence, thumbnails
/// only for the active tab (the other tab keeps its last ones), and app
/// icons fetched once and reused by source id — re-requested only when a
/// window id shows up that no icon fetch has seen yet.
function SourceGrid({
  pickedSourceId,
  onPick,
}: {
  pickedSourceId: string | null;
  onPick: (id: string) => void;
}) {
  const REFRESH_MS = 5000;
  const [tab, setTab] = useState<"screen" | "window">("screen");
  const [sources, setSources] = useState<CaptureSource[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  // Held across re-renders so the polling interval doesn't keep
  // reading stale `tab` state and so unmount cancels in-flight calls.
  const aliveRef = useRef(true);
  const tabRef = useRef(tab);
  // Per source id: last thumbnail / app icon we got. Ids an icon fetch has
  // already covered (icon or not) are remembered so a window without an
  // icon doesn't re-trigger icon fetches on every poll.
  const thumbsRef = useRef(new Map<string, string>());
  const iconsRef = useRef(new Map<string, string>());
  const iconCheckedRef = useRef(new Set<string>());
  const needIconsRef = useRef(true);
  const fetchRef = useRef<(() => Promise<void>) | null>(null);

  useEffect(() => {
    aliveRef.current = true;
    let inFlight = false;
    // A request (tab switch) that arrived mid-fetch runs once it lands.
    let again = false;
    const fetchOnce = async (): Promise<void> => {
      if (inFlight) {
        again = true;
        return;
      }
      inFlight = true;
      const withIcons = needIconsRef.current;
      try {
        const list = await window.decibell.capture.listSources({
          thumbnailWidth: 320,
          thumbnailHeight: 180,
          thumbnailKinds: [tabRef.current],
          fetchWindowIcons: withIcons,
        });
        if (!aliveRef.current) return;
        const thumbs = thumbsRef.current;
        const icons = iconsRef.current;
        const checked = iconCheckedRef.current;
        let unseenWindow = false;
        const merged = list.map((src) => {
          if (src.thumbnail) thumbs.set(src.id, src.thumbnail);
          if (withIcons) {
            checked.add(src.id);
            if (src.appIcon) icons.set(src.id, src.appIcon);
          } else if (src.kind === "window" && !checked.has(src.id)) {
            unseenWindow = true;
          }
          return {
            ...src,
            thumbnail: src.thumbnail || thumbs.get(src.id) || "",
            appIcon: src.appIcon || icons.get(src.id) || "",
          };
        });
        if (withIcons) needIconsRef.current = false;
        // A window appeared since the last icon fetch: get icons next poll.
        if (unseenWindow) needIconsRef.current = true;
        // Skip the re-render (and <img> re-decode) when nothing changed —
        // a static screen re-encodes to the identical JPEG.
        setSources((prev) =>
          prev.length === merged.length &&
          prev.every(
            (p, i) =>
              p.id === merged[i].id &&
              p.name === merged[i].name &&
              p.thumbnail === merged[i].thumbnail &&
              p.appIcon === merged[i].appIcon,
          )
            ? prev
            : merged,
        );
        setLoadError(null);
      } catch (e) {
        if (aliveRef.current) setLoadError(String(e));
      } finally {
        inFlight = false;
        if (again && aliveRef.current) {
          again = false;
          void fetchOnce();
        }
      }
    };
    fetchRef.current = fetchOnce;
    void fetchOnce();
    const id = window.setInterval(fetchOnce, REFRESH_MS);
    return () => {
      aliveRef.current = false;
      fetchRef.current = null;
      window.clearInterval(id);
    };
  }, []);

  // Switching tabs: fetch right away so the newly visible kind gets fresh
  // thumbnails instead of waiting up to REFRESH_MS.
  const firstTabRef = useRef(true);
  useEffect(() => {
    tabRef.current = tab;
    if (firstTabRef.current) {
      firstTabRef.current = false;
      return;
    }
    void fetchRef.current?.();
  }, [tab]);

  const screens = sources.filter((s) => s.kind === "screen");
  const windows = sources.filter((s) => s.kind === "window");
  const visible = tab === "screen" ? screens : windows;

  return (
    // Stacked: a capped grid above the settings. Side by side (lg): the
    // column fills the dialog's height and only the grid scrolls.
    <div className="flex shrink-0 flex-col px-5 pt-5 pb-4 lg:min-w-0 lg:flex-1 lg:pb-5">
      <div className="mb-3 flex shrink-0 rounded-md bg-bg-darkest p-[3px]">
        {([
          { value: "screen" as const, label: `Screens (${screens.length})` },
          { value: "window" as const, label: `Windows (${windows.length})` },
        ]).map((t) => (
          <button
            key={t.value}
            onClick={() => setTab(t.value)}
            className={`flex-1 rounded-sm px-3 py-[7px] text-[11px] font-semibold transition-all ${
              tab === t.value
                ? "bg-accent-mid text-accent-bright shadow-[0_0_6px_color-mix(in_srgb,var(--color-accent)_10%,transparent)]"
                : "text-text-muted hover:text-text-secondary"
            }`}
          >
            {t.label}
          </button>
        ))}
      </div>
      {loadError ? (
        <p className="py-6 text-center text-[12px] text-error">{loadError}</p>
      ) : sources.length === 0 ? (
        <p className="py-6 text-center text-[12px] text-text-muted">
          Loading sources…
        </p>
      ) : visible.length === 0 ? (
        <p className="py-6 text-center text-[12px] text-text-muted">
          No {tab === "screen" ? "screens" : "windows"} available.
        </p>
      ) : (
        <div className="grid max-h-[260px] auto-rows-max grid-cols-2 content-start gap-2.5 overflow-y-auto pr-1 lg:max-h-none lg:min-h-0 lg:flex-1">
          {visible.map((s) => {
            const picked = pickedSourceId === s.id;
            return (
              <button
                key={s.id}
                onClick={() => onPick(s.id)}
                className={`group flex flex-col overflow-hidden rounded-md border bg-bg-darkest transition-all ${
                  picked
                    ? "border-accent shadow-[0_0_0_1px_color-mix(in_srgb,var(--color-accent)_40%,transparent)]"
                    : "border-border hover:border-border-hover"
                }`}
              >
                <div className="relative aspect-video w-full bg-black">
                  {/* draggable=false stops Chromium from initiating an HTML5
                      drag on the thumbnail when the user click-and-holds —
                      the picker should feel like buttons, not images. */}
                  {s.thumbnail && (
                    <img
                      src={s.thumbnail}
                      alt={s.name}
                      draggable={false}
                      className="h-full w-full object-contain"
                    />
                  )}
                </div>
                <div className="flex items-center gap-2 px-2.5 py-2">
                  {s.appIcon && (
                    <img
                      src={s.appIcon}
                      alt=""
                      draggable={false}
                      className="h-4 w-4 shrink-0"
                    />
                  )}
                  <span
                    className={`truncate text-[12px] font-medium ${
                      picked ? "text-accent-bright" : "text-text-secondary"
                    }`}
                    title={s.name}
                  >
                    {s.name}
                  </span>
                </div>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

// Plan C: codec picker. Reads encodable codecs from codecSettingsStore
// (probed at app boot, filtered by user toggles). "Auto" = no enforcement;
// any explicit pick locks the stream to that codec.
function CodecPicker() {
  const streamSettings = useVoiceStore((s) => s.streamSettings);
  const setStreamSettings = useVoiceStore((s) => s.setStreamSettings);
  const { encodeCaps, load, loaded } = useCodecSettingsStore();
  useEffect(() => {
    if (!loaded) load().catch(() => {});
  }, [loaded, load]);

  const baseLabel = (c: VideoCodec): string => {
    switch (c) {
      case VideoCodec.AV1:
        return "AV1";
      case VideoCodec.H265:
        return "H.265";
      case VideoCodec.H264_HW:
        return "H.264";
      case VideoCodec.H264_SW:
        return "H.264 SW";
      default:
        return "Auto";
    }
  };

  const options: { value: VideoCodec; label: string }[] = [
    { value: VideoCodec.UNKNOWN, label: baseLabel(VideoCodec.UNKNOWN) },
    ...encodeCaps.map((c) => {
      const codec = c.codec as VideoCodec;
      const base = baseLabel(codec);
      // Only annotate the codec slots where the HW/SW distinction is
      // meaningful: AV1, H.265, and H264_HW. H264_SW is already labelled
      // "H.264 SW" by definition and the Auto entry has no probe data.
      const tag =
        codec !== VideoCodec.H264_SW && c.hardware !== undefined
          ? c.hardware
            ? " (HW)"
            : " (SW)"
          : "";
      return { value: codec, label: `${base}${tag}` };
    }),
  ];

  return (
    <div>
      <label
        className={FIELD_LABEL}
        title="Forcing a codec prevents viewers without that decoder from watching this stream."
      >
        Codec
      </label>
      <SegmentedControl
        options={options}
        value={streamSettings.enforcedCodec}
        onChange={(v) => setStreamSettings({ enforcedCodec: v })}
      />
      {streamSettings.enforcedCodec !== VideoCodec.UNKNOWN && (
        <p className="mt-1 text-[11px] text-text-muted">
          Viewers without this decoder won't be able to watch.
        </p>
      )}
    </div>
  );
}
