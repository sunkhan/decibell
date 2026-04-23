import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { useUiStore } from "../../../stores/uiStore";
import { saveSettings } from "../saveSettings";

// Presets tuned around typical broadband allowances. "Unlimited" is the top
// slot; everything else maps to whole MB/s for human legibility.
const PRESETS_MBPS: Array<{ label: string; mbps: number }> = [
  { label: "Unlimited", mbps: 0 },
  { label: "1 MB/s",    mbps: 1 },
  { label: "2 MB/s",    mbps: 2 },
  { label: "5 MB/s",    mbps: 5 },
  { label: "10 MB/s",   mbps: 10 },
  { label: "25 MB/s",   mbps: 25 },
  { label: "50 MB/s",   mbps: 50 },
  { label: "100 MB/s",  mbps: 100 },
];

function bpsToMbps(bps: number): number {
  return bps / (1024 * 1024);
}
function mbpsToBps(mbps: number): number {
  return Math.max(0, Math.round(mbps * 1024 * 1024));
}

function RateRow({
  label,
  description,
  valueBps,
  onChange,
}: {
  label: string;
  description: string;
  valueBps: number;
  onChange: (bps: number) => void;
}) {
  const mbps = bpsToMbps(valueBps);
  const [customStr, setCustomStr] = useState<string>(
    valueBps > 0 && !PRESETS_MBPS.some((p) => p.mbps === Math.round(mbps)) ? mbps.toFixed(1) : ""
  );
  useEffect(() => {
    if (valueBps > 0 && !PRESETS_MBPS.some((p) => p.mbps === Math.round(mbps))) {
      setCustomStr(mbps.toFixed(mbps < 10 ? 1 : 0));
    }
  }, [valueBps, mbps]);

  const preset = valueBps === 0
    ? 0
    : PRESETS_MBPS.find((p) => p.mbps === Math.round(mbps))?.mbps ?? -1;

  return (
    <div className="rounded-[10px] border border-border-divider bg-bg-light p-4">
      <div className="mb-0.5 text-[13px] font-medium text-text-primary">{label}</div>
      <div className="mb-3 text-[11.5px] text-text-muted">{description}</div>

      <div className="flex flex-wrap gap-2">
        {PRESETS_MBPS.map((p) => {
          const selected = preset === p.mbps;
          return (
            <button
              key={p.label}
              onClick={() => {
                onChange(mbpsToBps(p.mbps));
                setCustomStr("");
              }}
              className={`rounded-md px-3 py-1.5 text-[12px] font-medium transition-colors ${
                selected
                  ? "bg-accent text-white"
                  : "bg-bg-mid text-text-secondary hover:bg-surface-hover hover:text-text-primary"
              }`}
            >
              {p.label}
            </button>
          );
        })}
      </div>

      <div className="mt-3 flex items-center gap-2">
        <label className="text-[11.5px] text-text-muted">Custom:</label>
        <input
          type="number"
          min={0}
          step={0.5}
          inputMode="decimal"
          value={customStr}
          placeholder="e.g. 7.5"
          onChange={(e) => setCustomStr(e.target.value)}
          onBlur={() => {
            const n = parseFloat(customStr);
            if (!isNaN(n) && n >= 0) onChange(mbpsToBps(n));
          }}
          className="w-28 rounded-md border border-border bg-bg-mid px-2.5 py-1.5 text-[12px] text-text-primary outline-none transition-colors focus:border-accent"
        />
        <span className="text-[11.5px] text-text-muted">MB/s</span>
      </div>
    </div>
  );
}

export default function NetworkTab() {
  const uploadLimitBps = useUiStore((s) => s.uploadLimitBps);
  const downloadLimitBps = useUiStore((s) => s.downloadLimitBps);
  const setUploadLimitBps = useUiStore((s) => s.setUploadLimitBps);
  const setDownloadLimitBps = useUiStore((s) => s.setDownloadLimitBps);

  const apply = (nextUp: number, nextDown: number) => {
    setUploadLimitBps(nextUp);
    setDownloadLimitBps(nextDown);
    invoke("set_transfer_limits", {
      uploadBps: nextUp,
      downloadBps: nextDown,
    }).catch(console.error);
    saveSettings();
  };

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h3 className="mb-2 font-display text-[14px] font-semibold text-text-primary">
          Attachment transfer speed
        </h3>
        <p className="mb-4 text-[12px] text-text-muted">
          Limit how much of your connection the app uses when uploading or downloading files.
          Takes effect immediately, even mid-transfer. Voice and video streaming are not affected.
        </p>
      </div>

      <RateRow
        label="Upload limit"
        description="Per file. If you upload multiple files at once, each gets this cap."
        valueBps={uploadLimitBps}
        onChange={(bps) => apply(bps, downloadLimitBps)}
      />
      <RateRow
        label="Download limit"
        description="Per file. If you download multiple files at once, each gets this cap."
        valueBps={downloadLimitBps}
        onChange={(bps) => apply(uploadLimitBps, bps)}
      />
    </div>
  );
}
