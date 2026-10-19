import { useEffect } from "react";
import { useVoiceStore } from "../../../stores/voiceStore";
import { useCodecSettingsStore } from "../../../stores/codecSettingsStore";
import { VideoCodec } from "../../../types";
import SegmentedControl from "../../../components/SegmentedControl";
import { Icon } from "../../servers/tabs/settingsUi";
import { Field } from "../../servers/channelSettings/ui";
import { formatBitrate, resolutionLabel, stepBitrate } from "./presets";

const BITRATE_MIN = 1000;
const BITRATE_MAX = 30000;

function codecLabel(c: VideoCodec): string {
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
}

/// The raw stream knobs behind a disclosure: resolution, frame rate,
/// bitrate, codec and (with sound on) the sound bitrate. Any change here
/// that matches no preset shows as "Custom" above.
export default function AdvancedSettings({
  open,
  onToggle,
  showSoundBitrate,
  disabled,
}: {
  open: boolean;
  onToggle: () => void;
  showSoundBitrate: boolean;
  disabled?: boolean;
}) {
  const settings = useVoiceStore((s) => s.streamSettings);
  const setStreamSettings = useVoiceStore((s) => s.setStreamSettings);
  const { encodeCaps, load, loaded } = useCodecSettingsStore();
  useEffect(() => {
    if (!loaded) load().catch(() => {});
  }, [loaded, load]);

  // Plan C: encodable codecs from codecSettingsStore (probed at app boot,
  // filtered by user toggles). "Auto" = no enforcement; any explicit pick
  // locks the stream to that codec. Only a software encoder is tagged.
  const codecs = [
    { value: VideoCodec.UNKNOWN as VideoCodec, label: "Auto" },
    ...encodeCaps.map((c) => {
      const codec = c.codec as VideoCodec;
      const sw = codec !== VideoCodec.H264_SW && c.hardware === false;
      return { value: codec, label: `${codecLabel(codec)}${sw ? " SW" : ""}` };
    }),
  ];
  const forced = settings.enforcedCodec !== VideoCodec.UNKNOWN;
  const fill = ((settings.videoBitrateKbps - BITRATE_MIN) / (BITRATE_MAX - BITRATE_MIN)) * 100;

  const summary = [
    resolutionLabel(settings.resolution),
    `${settings.fps} fps`,
    formatBitrate(settings.videoBitrateKbps),
    forced ? codecLabel(settings.enforcedCodec) : "Auto codec",
  ].join(" · ");

  return (
    <div className="overflow-hidden rounded-lg border border-border-divider bg-bg-light">
      <button
        type="button"
        aria-expanded={open}
        onClick={onToggle}
        className="flex w-full items-center gap-3.5 px-4 py-3 text-left hover:bg-surface-hover"
      >
        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-bg-lighter text-text-secondary">
          <Icon name="sliders" size={17} />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-[14px] font-medium text-text-primary">Advanced</span>
          <span className="mt-0.5 block truncate text-[12px] tabular-nums text-text-muted">{summary}</span>
        </span>
        <Icon
          name="chevronDown"
          size={16}
          className={`text-text-muted transition-transform duration-150 ${open ? "rotate-180" : ""}`}
        />
      </button>
      {open && (
        <div className={`flex flex-col gap-5 border-t border-border-divider px-4 pt-4 pb-5 ${disabled ? "pointer-events-none opacity-60" : ""}`}>
          <Field label="Resolution">
            <SegmentedControl
              options={[
                { value: "720p" as const, label: "720p" },
                { value: "1080p" as const, label: "1080p" },
                { value: "source" as const, label: "Source", title: "Your screen or window's own size" },
              ]}
              value={settings.resolution}
              onChange={(v) =>
                setStreamSettings(
                  settings.quality === "custom"
                    ? { resolution: v }
                    : { resolution: v, videoBitrateKbps: stepBitrate(v, settings.quality) },
                )
              }
            />
          </Field>
          <Field label="Frame rate">
            <SegmentedControl
              options={[
                { value: 15 as const, label: "15 fps" },
                { value: 30 as const, label: "30 fps" },
                { value: 60 as const, label: "60 fps" },
                { value: 120 as const, label: "120 fps" },
              ]}
              value={settings.fps}
              onChange={(v) => setStreamSettings({ fps: v })}
            />
          </Field>
          <Field
            label="Video bitrate"
            aside={
              <span className="font-meta text-[12px] font-semibold tabular-nums text-text-secondary">
                {formatBitrate(settings.videoBitrateKbps)}
              </span>
            }
            hint="Higher looks better in motion and needs a faster upload."
          >
            <input
              type="range"
              min={BITRATE_MIN}
              max={BITRATE_MAX}
              step={500}
              aria-label="Video bitrate"
              value={settings.videoBitrateKbps}
              onChange={(e) =>
                setStreamSettings({ quality: "custom", videoBitrateKbps: Number(e.target.value) })
              }
              // The settings slider (AudioTab), with the run up to the thumb
              // filled in the accent.
              className="my-1.5 block h-1 w-full cursor-pointer appearance-none rounded-full accent-accent [&::-webkit-slider-thumb]:h-3.5 [&::-webkit-slider-thumb]:w-3.5 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-2 [&::-webkit-slider-thumb]:border-accent [&::-webkit-slider-thumb]:bg-bg-light [&::-webkit-slider-thumb]:shadow-[0_0_6px_var(--color-accent-mid)]"
              style={{
                background: `linear-gradient(to right, var(--color-accent) ${fill}%, var(--color-bg-lighter) 0)`,
              }}
            />
          </Field>
          <Field
            label="Codec"
            hint={
              forced ? (
                <span className="flex items-start gap-1.5 text-warning">
                  <Icon name="alert" size={13} className="mt-0.5" />
                  Viewers who can't play {codecLabel(settings.enforcedCodec)} won't be able to watch.
                </span>
              ) : (
                "Auto streams in a codec every viewer can play."
              )
            }
          >
            <SegmentedControl
              options={codecs}
              value={settings.enforcedCodec}
              onChange={(v) => setStreamSettings({ enforcedCodec: v })}
            />
          </Field>
          {showSoundBitrate && (
            <Field label="Sound bitrate">
              <SegmentedControl
                options={[
                  { value: 128 as const, label: "128 kbps" },
                  { value: 192 as const, label: "192 kbps" },
                ]}
                value={settings.audioBitrateKbps}
                onChange={(v) => setStreamSettings({ audioBitrateKbps: v })}
              />
            </Field>
          )}
        </div>
      )}
    </div>
  );
}
