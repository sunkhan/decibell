import { useState, useEffect } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";

interface CaptureSource {
  id: string;
  name: string;
  sourceType: "screen" | "window";
  width: number;
  height: number;
}

interface Props {
  serverId: string;
  channelId: string;
  onClose: () => void;
}

export default function CaptureSourcePicker({ serverId, channelId, onClose }: Props) {
  const [sources, setSources] = useState<CaptureSource[]>([]);
  const [tab, setTab] = useState<"screen" | "window">("screen");
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  const streamSettings = useVoiceStore((s) => s.streamSettings);
  const setStreamSettings = useVoiceStore((s) => s.setStreamSettings);

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
        shareAudio: streamSettings.shareAudio,
      });
      useVoiceStore.getState().setIsStreaming(true);
      onClose();
    } catch (e) {
      setError(String(e));
      setStarting(false);
    }
  };

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={(e) => e.target === e.currentTarget && onClose()}
    >
      <div className="w-[560px] rounded-xl border border-border bg-bg-secondary shadow-2xl">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border px-5 py-4">
          <h2 className="text-[15px] font-extrabold text-text-bright">
            Share Your Screen
          </h2>
          <button
            onClick={onClose}
            className="text-text-muted transition-colors hover:text-text-secondary"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

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
            <div className="grid grid-cols-2 gap-3 p-5">
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
                  <div className="flex h-20 items-center justify-center bg-bg-primary">
                    <span className="text-xs text-text-muted">
                      {source.width > 0
                        ? `${source.width} × ${source.height}`
                        : "Preview"}
                    </span>
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

        {/* Quality settings */}
        <div className="mx-5 flex gap-2.5 rounded-lg bg-bg-primary p-3">
          <div className="flex-1">
            <label className="mb-1 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
              Resolution
            </label>
            <select
              value={streamSettings.resolution}
              onChange={(e) => setStreamSettings({ resolution: e.target.value as any })}
              className="w-full rounded-md bg-surface-hover px-2.5 py-1.5 text-xs text-text-bright outline-none"
            >
              <option value="source">Source</option>
              <option value="1080p">1080p</option>
              <option value="720p">720p</option>
            </select>
          </div>
          <div className="flex-1">
            <label className="mb-1 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
              Frame Rate
            </label>
            <select
              value={streamSettings.fps}
              onChange={(e) => setStreamSettings({ fps: Number(e.target.value) as any })}
              className="w-full rounded-md bg-surface-hover px-2.5 py-1.5 text-xs text-text-bright outline-none"
            >
              <option value={60}>60 FPS</option>
              <option value={30}>30 FPS</option>
              <option value={15}>15 FPS</option>
            </select>
          </div>
          <div className="flex-1">
            <label className="mb-1 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
              Quality
            </label>
            <select
              value={streamSettings.quality}
              onChange={(e) => setStreamSettings({ quality: e.target.value as any })}
              className="w-full rounded-md bg-surface-hover px-2.5 py-1.5 text-xs text-text-bright outline-none"
            >
              <option value="high">High</option>
              <option value="medium">Medium</option>
              <option value="low">Low</option>
            </select>
          </div>
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
