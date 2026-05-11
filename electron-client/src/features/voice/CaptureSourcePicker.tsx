import { useState, useEffect, useCallback, useRef } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { useVoiceStore } from "../../stores/voiceStore";
import { useCodecSettingsStore } from "../../stores/codecSettingsStore";
import { VideoCodec, type CaptureSource } from "../../types";
import { playSound } from "../../utils/sounds";
import { startActiveStream, stopActiveStream } from "./streaming/StreamCapture";

interface Props {
  serverId: string;
  channelId: string;
  onClose: () => void;
}

/// Platforms where we render our own tabbed source picker. Linux is
/// excluded — getDisplayMedia goes through xdg-desktop-portal there
/// and the portal dialog is the picker. macOS uses Electron's
/// `useSystemPicker: true` to get the native ScreenCaptureKit dialog.
const NEEDS_CUSTOM_PICKER =
  typeof window !== "undefined" && window.decibell?.platform === "win32";

/** A segmented control — row of buttons where exactly one is selected. */
function SegmentedControl<T extends string | number>({
  options,
  value,
  onChange,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div className="flex rounded-[8px] bg-bg-darkest p-[3px]">
      {options.map((opt) => (
        <button
          key={String(opt.value)}
          onClick={() => onChange(opt.value)}
          className={`flex-1 rounded-[6px] px-3 py-[7px] text-[11px] font-semibold transition-all ${
            value === opt.value
              ? "bg-accent-mid text-accent-bright shadow-[0_0_6px_rgba(56,143,255,0.1)]"
              : "text-text-muted hover:text-text-secondary"
          }`}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}

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
      const stream = startActiveStream({
        codec,
        width: dims.width,
        height: dims.height,
        fps: streamSettings.fps,
        bitrateKbps: streamSettings.videoBitrateKbps,
        shareAudio: streamSettings.shareAudio,
        serverId,
        channelId,
        useNativeSize: streamSettings.resolution === "source",
        sourceId: pickedSourceId ?? undefined,
        onCaptureEnded: () => {
          useVoiceStore.getState().setIsStreaming(false);
          invoke("stop_screen_share", { serverId, channelId }).catch(() => {});
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

      // Native side: register the stream with the truthful dimensions.
      // The encoder.output's first chunk has already fired by now and
      // its send_video_frame call failed silently (no VideoEngine yet)
      // — that's one frame lost on the wire. Self-preview gets it via
      // the local fan-out, and remote watchers will request a fresh
      // keyframe via PLI on subscribe.
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
          initialCodec: codec,
          enforcedCodec: streamSettings.enforcedCodec || 0,
        });
      } catch (e) {
        await stopActiveStream();
        throw e;
      }

      useVoiceStore.getState().setIsStreaming(true);
      playSound("stream_start");
      handleClose();
    } catch (e) {
      setError(String(e));
      setStarting(false);
    }
  };

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{
        backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)",
      }}
      onClick={(e) => e.target === e.currentTarget && handleClose()}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        className="w-[560px] overflow-hidden rounded-2xl border border-border bg-bg-dark shadow-[0_24px_80px_rgba(0,0,0,0.6),0_0_0_1px_rgba(255,255,255,0.02)] transition-all duration-300"
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
      >
        {NEEDS_CUSTOM_PICKER ? (
          <SourceGrid
            pickedSourceId={pickedSourceId}
            onPick={setPickedSourceId}
          />
        ) : (
          <div className="px-6 py-8 text-center">
            <div className="mx-auto mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-accent-soft">
              <svg
                width="24"
                height="24"
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
            <p className="font-display text-[15px] font-semibold text-text-primary">
              Screen or window selection
            </p>
            <p className="mt-1.5 text-[13px] leading-relaxed text-text-muted">
              A system dialog will appear after you click Go Live to choose what to share.
            </p>
          </div>
        )}

        <div className="mx-5 mb-1 space-y-3 rounded-[10px] border border-border-divider bg-bg-light p-4">
          <div className="flex gap-3">
            <div className="flex-1">
              <label className="mb-2 block text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
                Resolution
              </label>
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
            <div className="flex-1">
              <label className="mb-2 block text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
                Frame rate
              </label>
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

          <CodecPicker />

          <div>
            <label className="mb-2 block text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
              Video quality
            </label>
            <div className="flex rounded-[8px] bg-bg-darkest p-[3px]">
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
                  className={`flex flex-1 flex-col items-center rounded-[6px] px-2 py-[7px] transition-all ${
                    streamSettings.quality === opt.key
                      ? "bg-accent-mid text-accent-bright shadow-[0_0_6px_rgba(56,143,255,0.1)]"
                      : "text-text-muted hover:text-text-secondary"
                  }`}
                >
                  <span className="text-[11px] font-semibold">{opt.label}</span>
                  {opt.sub && (
                    <span
                      className={`text-[9px] ${
                        streamSettings.quality === opt.key
                          ? "text-accent/60"
                          : "text-text-faint"
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
                  className="h-[6px] flex-1 cursor-pointer appearance-none rounded-full bg-bg-lighter accent-accent [&::-webkit-slider-thumb]:h-[16px] [&::-webkit-slider-thumb]:w-[16px] [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-mid [&::-webkit-slider-thumb]:shadow-[0_0_6px_rgba(56,143,255,0.3)]"
                />
                <span className="w-[60px] shrink-0 whitespace-nowrap text-right text-[11px] font-medium tabular-nums text-text-secondary">
                  {streamSettings.videoBitrateKbps >= 1000
                    ? `${(streamSettings.videoBitrateKbps / 1000).toFixed(streamSettings.videoBitrateKbps % 1000 === 0 ? 0 : 1)} Mbps`
                    : `${streamSettings.videoBitrateKbps} kbps`}
                </span>
              </div>
            )}
          </div>

          {streamSettings.shareAudio && (
            <div>
              <label className="mb-2 block text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
                Audio bitrate
              </label>
              <SegmentedControl
                options={[
                  { value: 128 as const, label: "128 kbps" },
                  { value: 192 as const, label: "192 kbps" },
                ]}
                value={streamSettings.audioBitrateKbps}
                onChange={(v) => setStreamSettings({ audioBitrateKbps: v })}
              />
            </div>
          )}
        </div>

        <div className="flex items-center justify-between px-5 py-4">
          <label className="flex cursor-pointer items-center gap-3 text-[13px] text-text-secondary">
            <button
              onClick={() =>
                setStreamSettings({ shareAudio: !streamSettings.shareAudio })
              }
              className={`relative h-[22px] w-[40px] shrink-0 rounded-full border transition-all ${
                streamSettings.shareAudio
                  ? "border-accent bg-accent shadow-[0_0_8px_rgba(56,143,255,0.22)]"
                  : "border-border bg-bg-lighter"
              }`}
            >
              <div
                className={`absolute top-[3px] h-[16px] w-[16px] rounded-full transition-all ${
                  streamSettings.shareAudio
                    ? "translate-x-[18px] bg-white"
                    : "translate-x-[3px] bg-text-muted"
                }`}
              />
            </button>
            Share audio
          </label>
          <button
            onClick={handleGoLive}
            disabled={starting || (NEEDS_CUSTOM_PICKER && !pickedSourceId)}
            className="rounded-[10px] bg-accent px-7 py-2.5 text-[13px] font-semibold text-white shadow-[0_2px_12px_rgba(56,143,255,0.22)] transition-all hover:bg-accent-hover hover:shadow-[0_4px_20px_rgba(56,143,255,0.3)] active:scale-[0.98] disabled:opacity-50 disabled:shadow-none"
          >
            {starting ? "Starting..." : "Go Live"}
          </button>
        </div>

        {error && <p className="px-5 pb-3 text-[12px] text-error">{error}</p>}
      </div>
    </div>,
    document.body,
  );
}

/// Tabbed grid of screens + windows, populated from Chromium's
/// desktopCapturer via the preload bridge. Thumbnails are PNG data URLs
/// (Chromium decodes them on assignment to <img>; no extra trip through
/// canvas). Re-polled at REFRESH_MS so the previews don't go stale while
/// the modal is open — Chromium re-snapshots the surfaces server-side,
/// which is the same path the in-browser screen-share dialog uses.
function SourceGrid({
  pickedSourceId,
  onPick,
}: {
  pickedSourceId: string | null;
  onPick: (id: string) => void;
}) {
  const REFRESH_MS = 2000;
  const [tab, setTab] = useState<"screen" | "window">("screen");
  const [sources, setSources] = useState<CaptureSource[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  // Held across re-renders so the polling interval doesn't keep
  // reading stale `tab` state and so unmount cancels in-flight calls.
  const aliveRef = useRef(true);

  useEffect(() => {
    aliveRef.current = true;
    const fetchOnce = async (): Promise<void> => {
      try {
        const list = await window.decibell.capture.listSources({
          thumbnailWidth: 320,
          thumbnailHeight: 180,
        });
        if (aliveRef.current) {
          setSources(list);
          setLoadError(null);
        }
      } catch (e) {
        if (aliveRef.current) setLoadError(String(e));
      }
    };
    void fetchOnce();
    const id = window.setInterval(fetchOnce, REFRESH_MS);
    return () => {
      aliveRef.current = false;
      window.clearInterval(id);
    };
  }, []);

  const screens = sources.filter((s) => s.kind === "screen");
  const windows = sources.filter((s) => s.kind === "window");
  const visible = tab === "screen" ? screens : windows;

  return (
    <div className="px-5 py-5">
      <p className="mb-3 font-display text-[14px] font-semibold text-text-primary">
        Choose what to share
      </p>
      <div className="mb-3 flex rounded-[8px] bg-bg-darkest p-[3px]">
        {([
          { value: "screen" as const, label: `Screens (${screens.length})` },
          { value: "window" as const, label: `Windows (${windows.length})` },
        ]).map((t) => (
          <button
            key={t.value}
            onClick={() => setTab(t.value)}
            className={`flex-1 rounded-[6px] px-3 py-[7px] text-[11px] font-semibold transition-all ${
              tab === t.value
                ? "bg-accent-mid text-accent-bright shadow-[0_0_6px_rgba(56,143,255,0.1)]"
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
        <div className="grid max-h-[260px] grid-cols-2 gap-2.5 overflow-y-auto pr-1">
          {visible.map((s) => {
            const picked = pickedSourceId === s.id;
            return (
              <button
                key={s.id}
                onClick={() => onPick(s.id)}
                className={`group flex flex-col overflow-hidden rounded-[8px] border bg-bg-darkest transition-all ${
                  picked
                    ? "border-accent shadow-[0_0_0_1px_rgba(56,143,255,0.4)]"
                    : "border-border hover:border-border-hover"
                }`}
              >
                <div className="relative aspect-video w-full bg-black">
                  {/* draggable=false stops Chromium from initiating an HTML5
                      drag on the thumbnail when the user click-and-holds —
                      the picker should feel like buttons, not images. */}
                  <img
                    src={s.thumbnail}
                    alt={s.name}
                    draggable={false}
                    className="h-full w-full object-contain"
                  />
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
                    className={`truncate text-[11.5px] font-medium ${
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
        className="mb-2 block text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted"
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
