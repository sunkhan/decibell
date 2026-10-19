import { useEffect, useRef } from "react";
import SegmentedControl from "../../components/SegmentedControl";
import { Icon } from "../servers/tabs/settingsUi";
import { useVoiceStore } from "../../stores/voiceStore";
import { useStreamAudioAppsStore } from "../../stores/streamAudioAppsStore";
import { StreamAudioMode } from "../../types";
import { setStreamAudioMode, toggleStreamAudioApp } from "./streamAudioFilter";

/// Apps start and stop playing; re-list on this cadence while mounted.
const POLL_MS = 2000;

const MODE_OPTIONS = [
  { value: StreamAudioMode.ALL, label: "All apps" },
  { value: StreamAudioMode.SELECTED, label: "Only these" },
  { value: StreamAudioMode.ALL_EXCEPT, label: "All except" },
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
  if (allMode) {
    const playing = apps.filter((a) => a.active).map((a) => a.name);
    hint = playing.length ? `Playing now: ${listNames(playing)}.` : "Nothing is playing right now.";
  } else if (audioMode === StreamAudioMode.SELECTED && audioApps.length === 0) {
    hint = "Tick the apps viewers should hear. Until then the stream is silent.";
  } else if (audioMode === StreamAudioMode.ALL_EXCEPT && audioApps.length === 0) {
    hint = "Tick the apps to leave out.";
  }

  return (
    <div className={`flex flex-col ${dense ? "gap-2" : "gap-2.5"}`}>
      <SegmentedControl options={MODE_OPTIONS} value={audioMode} onChange={setStreamAudioMode} />
      {!allMode && (
        <div
          className={`flex flex-col gap-px overflow-y-auto rounded-md border border-border-divider bg-bg-mid p-1 ${
            dense ? "max-h-[180px]" : "max-h-[200px]"
          }`}
        >
          {apps.length === 0 && notRunning.length === 0 ? (
            <div className="px-2 py-3 text-center text-[12px] text-text-muted">
              No apps are playing sound
            </div>
          ) : (
            <>
              {apps.map((app) => (
                <AppRow
                  key={app.id}
                  id={app.id}
                  name={app.name}
                  checked={ticked.has(app.id)}
                  active={app.active}
                  ownsWindow={app.ownsWindowSource}
                />
              ))}
              {notRunning.map((id) => (
                <AppRow key={`gone:${id}`} id={id} name={id} checked active={false} gone />
              ))}
            </>
          )}
        </div>
      )}
      {hint && <div className="text-[12px] leading-[1.55] text-text-muted">{hint}</div>}
    </div>
  );
}

function listNames(names: string[]): string {
  if (names.length <= 1) return names.join("");
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

function AppRow({
  id,
  name,
  checked,
  active,
  ownsWindow,
  gone,
}: {
  id: string;
  name: string;
  checked: boolean;
  active: boolean;
  ownsWindow?: boolean;
  gone?: boolean;
}) {
  return (
    <label className="flex cursor-pointer items-center gap-2.5 rounded-sm px-2 py-1.5 hover:bg-surface-hover">
      <input
        type="checkbox"
        checked={checked}
        onChange={() => toggleStreamAudioApp(id)}
        className="peer sr-only"
      />
      <span
        aria-hidden="true"
        className={`flex h-4 w-4 shrink-0 items-center justify-center rounded-sm border peer-focus-visible:shadow-ring ${
          checked ? "border-accent bg-accent text-on-accent" : "border-text-muted/50 bg-bg-lighter"
        }`}
      >
        {checked && <Icon name="check" size={11} strokeWidth={3.5} />}
      </span>
      <span
        className={`min-w-0 flex-1 truncate text-[13px] ${gone ? "text-text-muted" : "text-text-primary"}`}
      >
        {name}
      </span>
      {ownsWindow && (
        <span className="shrink-0 rounded-sm bg-accent-mid px-1.5 py-0.5 text-[11px] font-semibold text-accent-bright">
          this window
        </span>
      )}
      {gone ? (
        <span className="shrink-0 text-[11px] text-text-muted">not running</span>
      ) : (
        <PlayingBars active={active} />
      )}
    </label>
  );
}

/// Three bars while the app is making sound; a silent app shows none (the
/// slot is kept so the badges line up).
function PlayingBars({ active }: { active: boolean }) {
  return (
    <span
      title={active ? "Playing" : undefined}
      className="flex h-3 w-3.5 shrink-0 items-end justify-center gap-0.5"
    >
      {active &&
        [55, 100, 70].map((h, i) => (
          <span key={i} className="w-0.5 rounded-full bg-success" style={{ height: `${h}%` }} />
        ))}
    </span>
  );
}
