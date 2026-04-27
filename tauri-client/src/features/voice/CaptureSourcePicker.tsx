import { useState, useEffect, useCallback } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { useCodecSettingsStore } from "../../stores/codecSettingsStore";
import { VideoCodec } from "../../types";
import { playSound } from "../../utils/sounds";

interface CaptureSource {
  id: string;
  name: string;
  sourceType: "screen" | "window";
  width: number;
  height: number;
  thumbnail: string | null;
}

interface Props {
  serverId: string;
  channelId: string;
  onClose: () => void;
}

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

export default function CaptureSourcePicker({ serverId, channelId, onClose }: Props) {
  const [sources, setSources] = useState<CaptureSource[]>([]);
  const [tab, setTab] = useState<"screen" | "window">("screen");
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  const [visible, setVisible] = useState(false);
  const [closing, setClosing] = useState(false);

  const streamSettings = useVoiceStore((s) => s.streamSettings);
  const setStreamSettings = useVoiceStore((s) => s.setStreamSettings);

  // Animate in on mount
  useEffect(() => {
    requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
  }, []);

  // Animate out then call onClose
  const handleClose = useCallback(() => {
    if (closing) return;
    setClosing(true);
    setVisible(false);
  }, [closing]);

  const handleTransitionEnd = useCallback(() => {
    if (!visible && closing) onClose();
  }, [visible, closing, onClose]);

  useEffect(() => {
    invoke<CaptureSource[]>("list_capture_sources")
      .then((s) => {
        setSources(s);
        if (s.length > 0) setSelected(s[0].id);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  const screens = sources.filter((s) => s.sourceType === "screen");
  const windows = sources.filter((s) => s.sourceType === "window");
  const displayed = tab === "screen" ? screens : windows;

  const handleGoLive = async () => {
    if (!selected) return;
    setStarting(true);
    setError(null);
    try {
      await invoke("start_screen_share", {
        serverId,
        channelId,
        sourceId: selected,
        resolution: streamSettings.resolution,
        fps: streamSettings.fps,
        quality: streamSettings.quality,
        videoBitrateKbps: streamSettings.videoBitrateKbps,
        shareAudio: streamSettings.shareAudio,
        audioBitrateKbps: streamSettings.audioBitrateKbps,
        // Plan C: when the user explicitly picked a codec (not "Auto"),
        // forceCodec is the codec value AND streaming.rs marks it as
        // enforced on the wire so viewers without it see grayed Watch.
        forceCodec: streamSettings.enforcedCodec === VideoCodec.UNKNOWN
          ? null : streamSettings.enforcedCodec,
      });
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
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
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
        {/* Source selection — on Linux, the OS portal handles picking */}
        {!loading && sources.length === 1 && sources[0].id === "portal" ? (
          <div className="px-6 py-8 text-center">
            <div className="mx-auto mb-4 flex h-14 w-14 items-center justify-center rounded-full bg-accent-soft">
              <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="text-accent-bright">
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
        ) : (
          <>
            {/* Tabs */}
            <div className="flex gap-1 border-b border-border-divider px-5">
              <button
                onClick={() => setTab("screen")}
                className={`-mb-px border-b-2 px-4 py-3 text-[13px] font-medium transition-colors ${
                  tab === "screen"
                    ? "border-accent text-text-primary"
                    : "border-transparent text-text-muted hover:text-text-secondary"
                }`}
              >
                Screens
              </button>
              <button
                onClick={() => setTab("window")}
                className={`-mb-px border-b-2 px-4 py-3 text-[13px] font-medium transition-colors ${
                  tab === "window"
                    ? "border-accent text-text-primary"
                    : "border-transparent text-text-muted hover:text-text-secondary"
                }`}
              >
                Windows
              </button>
            </div>

            {/* Source grid */}
            <div className="grid max-h-[320px] grid-cols-2 gap-3 overflow-y-auto p-5">
              {loading && (
                <p className="col-span-2 py-10 text-center text-[13px] text-text-muted">
                  Loading sources...
                </p>
              )}
              {!loading && displayed.length === 0 && (
                <p className="col-span-2 py-10 text-center text-[13px] text-text-muted">
                  No {tab === "screen" ? "screens" : "windows"} found
                </p>
              )}
              {displayed.map((source) => (
                <button
                  key={source.id}
                  onClick={() => setSelected(source.id)}
                  className={`overflow-hidden rounded-[10px] text-left transition-all ${
                    selected === source.id
                      ? "ring-2 ring-accent ring-offset-1 ring-offset-bg-dark shadow-[0_0_12px_rgba(56,143,255,0.15)]"
                      : "ring-1 ring-border hover:ring-white/[0.12]"
                  }`}
                >
                  <div className="flex h-[120px] items-center justify-center overflow-hidden bg-bg-darkest">
                    {source.thumbnail ? (
                      <img
                        src={source.thumbnail}
                        alt={source.name}
                        className="h-full w-full object-cover"
                        draggable={false}
                      />
                    ) : (
                      <span className="text-[12px] text-text-muted">
                        {source.width > 0
                          ? `${source.width} × ${source.height}`
                          : "Preview"}
                      </span>
                    )}
                  </div>
                  <div
                    className={`px-3 py-2.5 text-[12px] font-medium ${
                      selected === source.id
                        ? "bg-accent-soft text-text-primary"
                        : "bg-bg-mid text-text-secondary"
                    }`}
                  >
                    {source.name}
                  </div>
                </button>
              ))}
            </div>
          </>
        )}

        {/* Stream settings */}
        <div className="mx-5 mb-1 space-y-3 rounded-[10px] border border-border-divider bg-bg-light p-4">
          {/* Row 1: Resolution, FPS */}
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
                    const presets = { low: isHighRes ? 6000 : 3000, medium: isHighRes ? 12000 : 6000, high: isHighRes ? 20000 : 10000 };
                    setStreamSettings({ videoBitrateKbps: presets[streamSettings.quality] });
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
                  { value: 60 as const, label: "60" },
                  { value: 30 as const, label: "30" },
                  { value: 15 as const, label: "15" },
                ]}
                value={streamSettings.fps}
                onChange={(v) => setStreamSettings({ fps: v })}
              />
            </div>
          </div>

          {/* Plan C: Codec — Auto + per-encodable codec from probe */}
          <CodecPicker />

          {/* Row 2: Video quality */}
          <div>
            <label className="mb-2 block text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
              Video quality
            </label>
            <div className="flex rounded-[8px] bg-bg-darkest p-[3px]">
              {(() => {
                const isHighRes = streamSettings.resolution === "source";
                return [
                  { key: "low" as const, label: "Low", sub: isHighRes ? "6 Mbps" : "3 Mbps", bitrate: isHighRes ? 6000 : 3000 },
                  { key: "medium" as const, label: "Medium", sub: isHighRes ? "12 Mbps" : "6 Mbps", bitrate: isHighRes ? 12000 : 6000 },
                  { key: "high" as const, label: "High", sub: isHighRes ? "20 Mbps" : "10 Mbps", bitrate: isHighRes ? 20000 : 10000 },
                  { key: "custom" as const, label: "Custom", sub: null, bitrate: null },
                ];
              })().map((opt) => (
                <button
                  key={opt.key}
                  onClick={() => {
                    if (opt.bitrate !== null) {
                      setStreamSettings({ quality: opt.key, videoBitrateKbps: opt.bitrate });
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
                    <span className={`text-[9px] ${
                      streamSettings.quality === opt.key ? "text-accent/60" : "text-text-faint"
                    }`}>
                      {opt.sub}
                    </span>
                  )}
                </button>
              ))}
            </div>

            {/* Custom bitrate slider */}
            {streamSettings.quality === "custom" && (
              <div className="mt-2.5 flex items-center gap-3 px-1">
                <input
                  type="range"
                  min={1000}
                  max={30000}
                  step={500}
                  value={streamSettings.videoBitrateKbps}
                  onChange={(e) => setStreamSettings({ videoBitrateKbps: Number(e.target.value) })}
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

          {/* Row 3: Audio bitrate */}
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

        {/* Bottom bar */}
        <div className="flex items-center justify-between px-5 py-4">
          <label className="flex cursor-pointer items-center gap-3 text-[13px] text-text-secondary">
            <button
              onClick={() => setStreamSettings({ shareAudio: !streamSettings.shareAudio })}
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
            disabled={!selected || starting}
            className="rounded-[10px] bg-accent px-7 py-2.5 text-[13px] font-semibold text-white shadow-[0_2px_12px_rgba(56,143,255,0.22)] transition-all hover:bg-accent-hover hover:shadow-[0_4px_20px_rgba(56,143,255,0.3)] active:scale-[0.98] disabled:opacity-50 disabled:shadow-none"
          >
            {starting ? "Starting..." : "Go Live"}
          </button>
        </div>

        {error && (
          <p className="px-5 pb-3 text-[12px] text-error">{error}</p>
        )}
      </div>
    </div>,
    document.body
  );
}

// ──────────────────────────────────────────────────────────────────────
// Plan C: Codec picker. Reads encodable codecs from codecSettingsStore
// (probed at app boot, filtered by user toggles). "Auto" is the default;
// selecting a specific codec marks the stream as enforced — viewers
// without that decoder can't subscribe.
// ──────────────────────────────────────────────────────────────────────
function CodecPicker() {
  const streamSettings = useVoiceStore((s) => s.streamSettings);
  const setStreamSettings = useVoiceStore((s) => s.setStreamSettings);
  const { encodeCaps, load, loaded } = useCodecSettingsStore();
  useEffect(() => { if (!loaded) load().catch(() => {}); }, [loaded, load]);

  const codecLabel = (c: VideoCodec): string => {
    switch (c) {
      case VideoCodec.AV1: return "Force AV1";
      case VideoCodec.H265: return "Force H.265";
      case VideoCodec.H264_HW: return "Force H.264";
      case VideoCodec.H264_SW: return "Force H.264 (software)";
      default: return "Auto (recommended)";
    }
  };

  return (
    <div>
      <label className="mb-2 block text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
        Codec
      </label>
      <select
        value={streamSettings.enforcedCodec}
        onChange={(e) =>
          setStreamSettings({ enforcedCodec: Number(e.target.value) as VideoCodec })
        }
        title="Forcing a codec prevents viewers without that decoder from watching this stream."
        className="w-full rounded-[8px] border border-border-divider bg-bg-darkest px-3 py-2 text-[13px] text-text-primary focus:outline-none focus:border-accent"
      >
        <option value={VideoCodec.UNKNOWN}>{codecLabel(VideoCodec.UNKNOWN)}</option>
        {encodeCaps.map((c) => (
          <option key={c.codec} value={c.codec}>
            {codecLabel(c.codec)}
          </option>
        ))}
      </select>
      {streamSettings.enforcedCodec !== VideoCodec.UNKNOWN && (
        <p className="mt-1 text-[11px] text-text-muted">
          Viewers without this decoder won't be able to watch.
        </p>
      )}
    </div>
  );
}
