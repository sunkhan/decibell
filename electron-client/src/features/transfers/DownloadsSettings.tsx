import { useEffect, useState } from "react";
import { useTransfersStore } from "../../stores/transfersStore";
import { saveSettings } from "../settings/saveSettings";
import Switch from "../../components/Switch";
import { pushDownloadConfig } from "./downloads";

/// "/home/me/…/Decibell downloads" — keep both ends of a long path.
function shortenMiddle(p: string, max = 56): string {
  if (p.length <= max) return p;
  const head = Math.ceil((max - 1) * 0.4);
  return `${p.slice(0, head)}…${p.slice(p.length - (max - 1 - head))}`;
}

/// Settings → Network → Downloads: the folder downloads land in, and
/// whether to ask every time instead.
export default function DownloadsSettings() {
  const dir = useTransfersStore((s) => s.downloadDir);
  const ask = useTransfersStore((s) => s.askDownloadLocation);
  const [defaultDir, setDefaultDir] = useState("");

  useEffect(() => {
    window.decibell.downloads
      .defaultFolder()
      .then(setDefaultDir)
      .catch(() => {});
  }, []);

  const commit = () => {
    pushDownloadConfig();
    saveSettings();
  };
  const change = async () => {
    const picked = await window.decibell.downloads.pickFolder().catch(() => null);
    if (!picked) return;
    useTransfersStore.getState().setDownloadDir(picked === defaultDir ? "" : picked);
    commit();
  };
  const reset = () => {
    useTransfersStore.getState().setDownloadDir("");
    commit();
  };
  const toggleAsk = () => {
    useTransfersStore.getState().setAskDownloadLocation(!ask);
    commit();
  };
  const shown = dir || defaultDir;

  return (
    <div className="flex flex-col gap-3">
      <div>
        <h3 className="mb-2 font-display text-[14px] font-semibold text-text-primary">Downloads</h3>
        <p className="text-[12px] text-text-muted">
          Where the attachments you download are saved. Progress and history are in the
          Transfers panel in the title bar (Ctrl+J).
        </p>
      </div>

      <div className="rounded-md border border-border-divider bg-bg-light p-4">
        <div className="mb-0.5 text-[13px] font-medium text-text-primary">Location</div>
        <div className="mb-3 text-[12px] text-text-muted">
          {dir ? "A folder you chose." : "Your system's Downloads folder."}
        </div>
        <div className="flex items-center gap-2">
          <div
            title={shown}
            className="min-w-0 flex-1 truncate rounded-sm border border-border bg-bg-mid px-2.5 py-1.5 text-[12px] text-text-secondary"
          >
            {shortenMiddle(shown)}
          </div>
          <button
            type="button"
            onClick={() => void change()}
            className="shrink-0 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
          >
            Change…
          </button>
          {dir && (
            <button
              type="button"
              onClick={reset}
              className="shrink-0 rounded-sm bg-bg-mid px-4 py-2 text-[13px] font-semibold text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
            >
              Reset
            </button>
          )}
        </div>
      </div>

      <div className="flex items-center justify-between rounded-md border border-border-divider bg-bg-light px-4 py-3.5 transition-colors hover:bg-bg-lighter">
        <div className="pr-4">
          <div className="text-[14px] font-medium text-text-primary">Ask where to save each file</div>
          <div className="mt-1 text-[12px] leading-[1.55] text-text-muted">
            Show a save dialog for every download instead of saving straight to the folder above.
          </div>
        </div>
        <Switch checked={ask} onToggle={toggleAsk} label="Ask where to save each file" />
      </div>
    </div>
  );
}
