import { useState, useEffect, useCallback } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";

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
    <div className="flex rounded-lg bg-bg-tertiary p-0.5">
      {options.map((opt) => (
        <button
          key={String(opt.value)}
          onClick={() => onChange(opt.value)}
          className={`flex-1 rounded-md px-2.5 py-1.5 text-[11px] font-semibold transition-colors ${
            value === opt.value
              ? "bg-accent/15 text-accent"
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
      });
      useVoiceStore.getState().setIsStreaming(true);
      handleClose();
    } catch (e) {
      setError(String(e));
      setStarting(false);
    }
  };

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.6)" : "rgba(0,0,0,0)" }}
      onClick={(e) => e.target === e.currentTarget && handleClose()}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        className="w-[560px] rounded-xl border border-border bg-bg-secondary shadow-2xl transition-all duration-300"
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
      >
        {/* Source selection — on Linux, the OS portal handles picking */}
        {!loading && sources.length === 1 && sources[0].id === "portal" ? (
          <div className="px-5 py-6 text-center">
            <div className="mx-auto mb-3 flex h-12 w-12 items-center justify-center rounded-xl bg-accent/10 text-xl">
              🖥
            </div>
            <p className="text-sm font-semibold text-text-bright">
              Screen or window selection
            </p>
            <p className="mt-1 text-xs text-text-muted">
              A system dialog will appear after you click Go Live to choose what to share.
            </p>
          </div>
        ) : (
          <>
            {/* Tabs */}
            <div className="flex border-b-2 border-border px-5">
              <button
                onClick={() => setTab("screen")}
                className={`-mb-[2px] border-b-2 px-4 py-2.5 text-xs font-bold transition-colors ${
                  tab === "screen"
                    ? "border-accent text-accent"
                    : "border-transparent text-text-muted hover:text-text-secondary"
                }`}
              >
                Screens
              </button>
              <button
                onClick={() => setTab("window")}
                className={`-mb-[2px] border-b-2 px-4 py-2.5 text-xs font-bold transition-colors ${
                  tab === "window"
                    ? "border-accent text-accent"
                    : "border-transparent text-text-muted hover:text-text-secondary"
                }`}
              >
                Windows
              </button>
            </div>

            {/* Source grid */}
            <div className="grid max-h-[320px] grid-cols-2 gap-3 overflow-y-auto p-5">
              {loading && (
                <p className="col-span-2 py-8 text-center text-sm text-text-muted">
                  Loading sources...
                </p>
              )}
              {!loading && displayed.length === 0 && (
                <p className="col-span-2 py-8 text-center text-sm text-text-muted">
                  No {tab === "screen" ? "screens" : "windows"} found
                </p>
              )}
              {displayed.map((source) => (
                <button
                  key={source.id}
                  onClick={() => setSelected(source.id)}
                  className={`overflow-hidden rounded-lg border-2 text-left transition-all ${
                    selected === source.id
                      ? "border-accent"
                      : "border-border hover:border-text-muted"
                  }`}
                >
                  <div className="flex h-[120px] items-center justify-center bg-bg-primary overflow-hidden">
                    {source.thumbnail ? (
                      <img
                        src={source.thumbnail}
                        alt={source.name}
                        className="h-full w-full object-cover"
                        draggable={false}
                      />
                    ) : (
                      <span className="text-xs text-text-muted">
                        {source.width > 0
                          ? `${source.width} × ${source.height}`
                          : "Preview"}
                      </span>
                    )}
                  </div>
                  <div
                    className={`px-3 py-2 text-[11px] font-semibold ${
                      selected === source.id
                        ? "bg-accent/10 text-text-bright"
                        : "text-text-secondary"
                    }`}
                  >
                    {source.name}
                  </div>
                </button>
              ))}
            </div>
          </>
        )}

        {/* Stream settings — segmented controls */}
        <div className="mx-5 space-y-2.5 rounded-lg bg-bg-primary p-3.5">
          {/* Row 1: Resolution, FPS */}
          <div className="flex gap-2.5">
            <div className="flex-1">
              <label className="mb-1.5 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
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
                  // Auto-scale bitrate preset when switching resolution
                  if (streamSettings.quality !== "custom") {
                    const isHighRes = v === "source";
                    const presets = { low: isHighRes ? 6000 : 3000, medium: isHighRes ? 12000 : 6000, high: isHighRes ? 20000 : 10000 };
                    setStreamSettings({ videoBitrateKbps: presets[streamSettings.quality] });
                  }
                }}
              />
            </div>
            <div className="flex-1">
              <label className="mb-1.5 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
                Frame Rate
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

          {/* Row 2: Video quality — presets + custom */}
          <div>
            <label className="mb-1.5 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
              Video Quality
            </label>
            <div className="flex rounded-lg bg-bg-tertiary p-0.5">
              {(() => {
                // Scale bitrate presets based on resolution — 1440p+ needs more headroom
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
                  className={`flex flex-1 flex-col items-center rounded-md px-2 py-1.5 transition-colors ${
                    streamSettings.quality === opt.key
                      ? "bg-accent/15 text-accent"
                      : "text-text-muted hover:text-text-secondary"
                  }`}
                >
                  <span className="text-[11px] font-semibold">{opt.label}</span>
                  {opt.sub && (
                    <span className={`text-[9px] ${
                      streamSettings.quality === opt.key ? "text-accent/60" : "text-text-muted"
                    }`}>
                      {opt.sub}
                    </span>
                  )}
                </button>
              ))}
            </div>

            {/* Custom bitrate slider */}
            {streamSettings.quality === "custom" && (
              <div className="mt-2 flex items-center gap-3 px-1">
                <input
                  type="range"
                  min={1000}
                  max={30000}
                  step={500}
                  value={streamSettings.videoBitrateKbps}
                  onChange={(e) => setStreamSettings({ videoBitrateKbps: Number(e.target.value) })}
                  className="custom-slider h-1.5 flex-1 cursor-pointer appearance-none rounded-full bg-surface-active accent-accent"
                />
                <span className="w-[60px] shrink-0 whitespace-nowrap text-right text-[11px] font-semibold tabular-nums text-text-secondary">
                  {streamSettings.videoBitrateKbps >= 1000
                    ? `${(streamSettings.videoBitrateKbps / 1000).toFixed(streamSettings.videoBitrateKbps % 1000 === 0 ? 0 : 1)} Mbps`
                    : `${streamSettings.videoBitrateKbps} kbps`}
                </span>
              </div>
            )}
          </div>

          {/* Row 3: Audio bitrate (only visible when audio sharing is on) */}
          {streamSettings.shareAudio && (
            <div>
              <label className="mb-1.5 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
                Audio Bitrate
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
          <label className="flex cursor-pointer items-center gap-2.5 text-xs text-text-secondary">
            <div
              onClick={() => setStreamSettings({ shareAudio: !streamSettings.shareAudio })}
              className={`relative h-5 w-9 rounded-full transition-colors ${
                streamSettings.shareAudio ? "bg-accent" : "bg-surface-hover"
              }`}
            >
              <div
                className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition-transform ${
                  streamSettings.shareAudio ? "translate-x-4" : "translate-x-0.5"
                }`}
              />
            </div>
            Share audio
          </label>
          <button
            onClick={handleGoLive}
            disabled={!selected || starting}
            className="rounded-lg bg-accent px-6 py-2 text-[13px] font-bold text-white transition-colors hover:bg-accent-hover disabled:opacity-50"
          >
            {starting ? "Starting..." : "Go Live"}
          </button>
        </div>

        {error && (
          <p className="px-5 pb-3 text-xs text-error">{error}</p>
        )}
      </div>
    </div>,
    document.body
  );
}
