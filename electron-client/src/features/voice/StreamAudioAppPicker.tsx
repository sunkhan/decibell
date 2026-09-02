import { useEffect, useRef } from "react";
import SegmentedControl from "../../components/SegmentedControl";
import { useVoiceStore } from "../../stores/voiceStore";
import { useStreamAudioAppsStore } from "../../stores/streamAudioAppsStore";
import { StreamAudioMode } from "../../types";
import { setStreamAudioMode, toggleStreamAudioApp } from "./streamAudioFilter";

/// Apps start and stop playing; re-list on this cadence while mounted.
const POLL_MS = 2000;

const MODE_OPTIONS = [
  { value: StreamAudioMode.SELECTED, label: "Selected apps" },
  { value: StreamAudioMode.ALL_EXCEPT, label: "All except" },
  { value: StreamAudioMode.ALL, label: "All apps" },
];

interface Props {
  /// desktopCapturer id of the picked / active video source. Windows uses
  /// it to flag the app that owns that window; other platforms ignore it.
  sourceId?: string;
  /// Tighter list for the live popover.
  dense?: boolean;
}

/// Mode control + checkbox list of applications with an audio output. Ticks
/// and mode persist via voiceStore.streamSettings and, while streaming, apply
/// to the running capture immediately (see streamAudioFilter.ts).
export default function StreamAudioAppPicker({ sourceId, dense }: Props) {
  const audioMode = useVoiceStore((s) => s.streamSettings.audioMode);
  const audioApps = useVoiceStore((s) => s.streamSettings.audioApps);
  const apps = useStreamAudioAppsStore((s) => s.apps);
  const supported = useStreamAudioAppsStore((s) => s.supported);
  const refresh = useStreamAudioAppsStore((s) => s.refresh);

  // Poll while mounted; the store skips its set when nothing changed.
  const aliveRef = useRef(true);
  useEffect(() => {
    aliveRef.current = true;
    void refresh(sourceId);
    const timer = setInterval(() => {
      if (aliveRef.current) void refresh(sourceId);
    }, POLL_MS);
    return () => {
      aliveRef.current = false;
      clearInterval(timer);
    };
  }, [refresh, sourceId]);

  if (!supported) return null;

  const allMode = audioMode === StreamAudioMode.ALL;
  const ticked = new Set(audioApps);
  const listed = new Set(apps.map((a) => a.id));
  // Remembered ticks for apps that aren't running right now — shown greyed
  // so a stale tick can be cleared (a blacklisted app stays remembered).
  const notRunning = allMode ? [] : audioApps.filter((id) => !listed.has(id));

  let hint: string | null = null;
  if (audioMode === StreamAudioMode.SELECTED && audioApps.length === 0) {
    hint = "Nothing selected — the stream will be silent.";
  } else if (audioMode === StreamAudioMode.ALL_EXCEPT && audioApps.length === 0) {
    hint = "Nothing excluded — every app is streamed.";
  }

  return (
    <div className={`flex flex-col ${dense ? "gap-2" : "gap-2.5"}`}>
      <SegmentedControl options={MODE_OPTIONS} value={audioMode} onChange={setStreamAudioMode} />
      <div
        className={`flex flex-col gap-0.5 overflow-y-auto rounded-md border border-border bg-bg-darkest p-1 ${
          dense ? "max-h-[180px]" : "max-h-[200px]"
        }`}
      >
        {apps.length === 0 && notRunning.length === 0 ? (
          <div className="px-2 py-3 text-center text-[11px] text-text-muted">
            No apps are playing audio
          </div>
        ) : (
          <>
            {apps.map((app) => (
              <AppRow
                key={app.id}
                id={app.id}
                name={app.name}
                checked={allMode || ticked.has(app.id)}
                disabled={allMode}
                active={app.active}
                ownsWindow={app.ownsWindowSource}
              />
            ))}
            {notRunning.map((id) => (
              <AppRow key={`gone:${id}`} id={id} name={id} checked disabled={false} active={false} gone />
            ))}
          </>
        )}
      </div>
      {hint && <div className="text-[11px] text-text-muted">{hint}</div>}
    </div>
  );
}

function AppRow({
  id,
  name,
  checked,
  disabled,
  active,
  ownsWindow,
  gone,
}: {
  id: string;
  name: string;
  checked: boolean;
  disabled: boolean;
  active: boolean;
  ownsWindow?: boolean;
  gone?: boolean;
}) {
  return (
    <label
      className={`flex items-center gap-2.5 rounded-sm px-2 py-1.5 transition-colors ${
        disabled ? "cursor-default opacity-60" : "cursor-pointer hover:bg-surface-hover"
      }`}
    >
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={() => toggleStreamAudioApp(id)}
        className="accent-[var(--color-accent)]"
      />
      <span
        className={`h-1.5 w-1.5 shrink-0 rounded-full ${active ? "bg-success" : "bg-text-faint"}`}
        title={active ? "Playing" : "Silent"}
      />
      <span
        className={`min-w-0 flex-1 truncate text-[12px] ${gone ? "text-text-muted" : "text-text-primary"}`}
      >
        {name}
      </span>
      {ownsWindow && (
        <span className="shrink-0 rounded-sm bg-accent-mid px-1.5 py-0.5 text-[10px] font-semibold text-accent-bright">
          this window
        </span>
      )}
      {gone && <span className="shrink-0 text-[10px] text-text-faint">not running</span>}
    </label>
  );
}
