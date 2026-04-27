import { useEffect } from "react";
import { useCodecSettingsStore } from "../../../stores/codecSettingsStore";
import { VideoCodec, type CodecCapability } from "../../../types";

const codecLabel = (c: VideoCodec): string => {
  switch (c) {
    case VideoCodec.AV1:    return "AV1";
    case VideoCodec.H265:   return "H.265 / HEVC";
    case VideoCodec.H264_HW: return "H.264 (hardware)";
    case VideoCodec.H264_SW: return "H.264 (software)";
    default:                 return "Unknown";
  }
};

const formatCap = (c: CodecCapability) =>
  `${codecLabel(c.codec)} — up to ${c.maxWidth}×${c.maxHeight} @ ${c.maxFps}fps`;

interface ToggleRowProps {
  label: string;
  hint: string;
  checked: boolean;
  disabled: boolean;
  disabledHint?: string;
  onToggle: () => void;
}

function ToggleRow({ label, hint, checked, disabled, disabledHint, onToggle }: ToggleRowProps) {
  return (
    <div className="flex items-center justify-between rounded-[10px] border border-border-divider bg-bg-light px-4 py-3.5 transition-colors hover:bg-bg-lighter">
      <div className="pr-4">
        <div className={`text-[14px] font-medium ${disabled ? "text-text-muted" : "text-text-primary"}`}>
          {label}
        </div>
        <div className="mt-1 text-[12px] leading-relaxed text-text-muted">
          {disabled && disabledHint ? disabledHint : hint}
        </div>
      </div>
      <button
        onClick={disabled ? undefined : onToggle}
        disabled={disabled}
        className={`relative h-[22px] w-[40px] shrink-0 rounded-full border transition-all ${
          disabled
            ? "cursor-not-allowed border-border bg-bg-lighter opacity-50"
            : checked
            ? "border-accent bg-accent shadow-[0_0_8px_rgba(56,143,255,0.22)]"
            : "border-border bg-bg-lighter"
        }`}
      >
        <div
          className={`absolute top-[3px] h-[16px] w-[16px] rounded-full transition-all ${
            checked
              ? "translate-x-[18px] bg-white"
              : "translate-x-[3px] bg-text-muted"
          }`}
        />
      </button>
    </div>
  );
}

export default function CodecsTab() {
  const {
    useAv1, useH265, encodeCaps, decodeCaps, loaded, loadingRefresh,
    load, setUseAv1, setUseH265, refresh,
  } = useCodecSettingsStore();

  useEffect(() => { load().catch((e) => console.error("[codecs] load failed:", e)); }, [load]);

  const hasAv1Encode = encodeCaps.some((c) => c.codec === VideoCodec.AV1);
  const hasH265Encode = encodeCaps.some((c) => c.codec === VideoCodec.H265);

  return (
    <div className="flex flex-col gap-3">
      <ToggleRow
        label="Use AV1 codec when available"
        hint="Auto-pick AV1 for screen sharing when your hardware supports it. Best quality at lower bitrates."
        disabledHint="Your hardware does not support AV1 encoding."
        checked={useAv1 && hasAv1Encode}
        disabled={!hasAv1Encode}
        onToggle={() => setUseAv1(!useAv1).catch(console.error)}
      />
      <ToggleRow
        label="Use H.265 / HEVC codec when available"
        hint="Auto-pick H.265 for screen sharing when your hardware supports it. Better than H.264, broader hardware support than AV1."
        disabledHint="Your hardware does not support H.265 encoding."
        checked={useH265 && hasH265Encode}
        disabled={!hasH265Encode}
        onToggle={() => setUseH265(!useH265).catch(console.error)}
      />

      <div className="mt-2 flex items-center gap-3">
        <button
          onClick={() => refresh().catch(console.error)}
          disabled={loadingRefresh}
          className="rounded-[8px] border border-border-divider bg-bg-light px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter disabled:cursor-not-allowed disabled:opacity-50"
        >
          {loadingRefresh ? "Refreshing…" : "Refresh codec capabilities"}
        </button>
        <span className="text-[12px] text-text-muted">
          Re-probe after a GPU/driver change.
        </span>
      </div>

      {loaded && (
        <div className="mt-4 grid grid-cols-2 gap-3">
          <div className="rounded-[10px] border border-border-divider bg-bg-light p-4">
            <div className="mb-2 text-[12px] font-semibold uppercase tracking-wider text-text-muted">
              Detected encoders
            </div>
            {encodeCaps.length === 0 ? (
              <div className="text-[12px] text-text-muted">None detected.</div>
            ) : (
              <ul className="space-y-1.5">
                {encodeCaps.map((c) => (
                  <li key={`enc-${c.codec}`} className="text-[12.5px] text-text-primary">
                    {formatCap(c)}
                  </li>
                ))}
              </ul>
            )}
          </div>
          <div className="rounded-[10px] border border-border-divider bg-bg-light p-4">
            <div className="mb-2 text-[12px] font-semibold uppercase tracking-wider text-text-muted">
              Detected decoders
            </div>
            {decodeCaps.length === 0 ? (
              <div className="text-[12px] text-text-muted">None detected.</div>
            ) : (
              <ul className="space-y-1.5">
                {decodeCaps.map((c) => (
                  <li key={`dec-${c.codec}`} className="text-[12.5px] text-text-primary">
                    {formatCap(c)}
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      )}

      <p className="mt-2 text-[11.5px] text-text-muted">
        Disabling a codec removes it from the list advertised to peers, so streams
        you start will skip it during auto-selection. You can still receive streams
        that use it (decoding is independent).
      </p>
    </div>
  );
}
