import { useVoiceStore } from "../../../stores/voiceStore";
import { Icon } from "../../servers/tabs/settingsUi";
import {
  STREAM_PRESETS,
  formatBitrate,
  matchPreset,
  presetSettings,
  resolutionLabel,
} from "./presets";

/// Four named quality cards, two by two. Settings that match none of them
/// (changed under Advanced) leave every card unpicked and say so underneath.
export default function QualityPresets({ disabled }: { disabled?: boolean }) {
  const settings = useVoiceStore((s) => s.streamSettings);
  const setStreamSettings = useVoiceStore((s) => s.setStreamSettings);
  const active = matchPreset(settings);

  return (
    <div>
      <div role="radiogroup" aria-label="Stream quality" className="grid grid-cols-2 gap-2">
        {STREAM_PRESETS.map((p) => {
          const on = active?.id === p.id;
          return (
            <button
              key={p.id}
              type="button"
              role="radio"
              aria-checked={on}
              disabled={disabled}
              onClick={() => setStreamSettings(presetSettings(p))}
              className={`flex min-w-0 items-center gap-3 rounded-lg border px-3 py-2.5 text-left disabled:opacity-60 ${
                on
                  ? "border-accent bg-accent-soft"
                  : "border-border-divider bg-bg-light enabled:hover:bg-bg-lighter"
              }`}
            >
              <Icon name={p.icon} size={18} className={on ? "text-accent" : "text-text-muted"} />
              <span className="flex w-full min-w-0 flex-col gap-0.5">
                <span className={`truncate text-[13px] font-semibold ${on ? "text-text-bright" : "text-text-primary"}`}>
                  {p.label}
                </span>
                <span className="truncate text-[12px] tabular-nums text-text-muted">{p.spec}</span>
              </span>
            </button>
          );
        })}
      </div>
      <p className="mt-2.5 text-[12px] leading-[1.55] text-text-muted">
        {active ? (
          active.blurb
        ) : (
          <>
            <span className="font-medium text-text-secondary">Custom:</span>{" "}
            {resolutionLabel(settings.resolution)} · {settings.fps} fps · {formatBitrate(settings.videoBitrateKbps)}, set
            under Advanced.
          </>
        )}
      </p>
    </div>
  );
}
