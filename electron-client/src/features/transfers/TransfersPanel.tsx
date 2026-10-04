import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import SegmentedControl from "../../components/SegmentedControl";
import { useTransfersStore, type TransfersTab } from "../../stores/transfersStore";
import { useUiStore } from "../../stores/uiStore";
import type { DownloadView } from "../../types";
import DownloadRow from "./DownloadRow";
import UploadRow from "./UploadRow";
import { refreshDownloads } from "./downloads";

const WIDTH = 360;
const MARGIN = 8;

function capLabel(bps: number): string {
  const mbps = Math.round((bps / (1024 * 1024)) * 100) / 100;
  return `${mbps} MB/s`;
}

interface MenuState {
  x: number;
  y: number;
  d: DownloadView;
}

/// The Transfers popover: Downloads | Uploads tabs, the list, and a
/// footer with the speed cap, the download folder and Clear. Anchored
/// under the title-bar button, right-aligned; closes on outside click
/// and Escape (clicks on the button itself are the toggle's).
export default function TransfersPanel({
  anchorEl,
  onClose,
  onOpenFile,
}: {
  anchorEl: HTMLElement;
  onClose: (refocus: boolean) => void;
  onOpenFile: (d: DownloadView) => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const tab = useTransfersStore((s) => s.tab);
  const setTab = useTransfersStore((s) => s.setTab);
  const downloads = useTransfersStore((s) => s.downloads);
  const uploads = useTransfersStore((s) => s.uploads);
  const downloadCap = useUiStore((s) => s.downloadLimitBps);
  const uploadCap = useUiStore((s) => s.uploadLimitBps);
  const [now, setNow] = useState(() => Date.now());
  const [menu, setMenu] = useState<MenuState | null>(null);

  useEffect(() => {
    // Fresh list on open: main re-checks finished files on disk.
    void refreshDownloads();
    ref.current?.focus();
    const tick = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(tick);
  }, []);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (ref.current && !ref.current.contains(t) && !anchorEl.contains(t)) onClose(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (menu) setMenu(null);
      else onClose(true);
    };
    const timer = window.setTimeout(() => {
      document.addEventListener("mousedown", onDown);
      document.addEventListener("keydown", onKey);
    }, 0);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [anchorEl, onClose, menu]);

  const downloadsLive = useMemo(
    () => downloads.filter((d) => d.state === "active" || d.state === "paused").length,
    [downloads],
  );
  const uploadsLive = useMemo(
    () => uploads.filter((u) => u.state === "uploading" || u.state === "paused").length,
    [uploads],
  );
  const finished = useMemo(
    () =>
      tab === "downloads"
        ? downloads.some((d) => d.state === "done" || d.state === "failed" || d.state === "cancelled")
        : uploads.some((u) => u.state !== "uploading" && u.state !== "paused"),
    [tab, downloads, uploads],
  );

  const rect = anchorEl.getBoundingClientRect();
  const style: React.CSSProperties = {
    top: rect.bottom + 6,
    right: Math.max(MARGIN, window.innerWidth - rect.right - MARGIN),
    width: WIDTH,
  };

  const openSettings = () => {
    onClose(false);
    useUiStore.getState().setSettingsTab("network");
    useUiStore.getState().openModal("settings");
  };
  const cap = tab === "downloads" ? downloadCap : uploadCap;

  return createPortal(
    <div
      ref={ref}
      role="dialog"
      aria-label="Transfers"
      tabIndex={-1}
      style={style}
      className="fixed z-[100] flex flex-col overflow-hidden rounded-lg border border-border bg-bg-secondary shadow-modal outline-none animate-[dropIn_0.12s_ease_both]"
      onClick={() => setMenu(null)}
    >
      <div className="px-3 pb-2 pt-3">
        <SegmentedControl<TransfersTab>
          value={tab}
          onChange={setTab}
          options={[
            { value: "downloads", label: downloadsLive > 0 ? `Downloads · ${downloadsLive}` : "Downloads" },
            { value: "uploads", label: uploadsLive > 0 ? `Uploads · ${uploadsLive}` : "Uploads" },
          ]}
        />
      </div>

      <div className="max-h-[384px] overflow-y-auto px-1.5 pb-1.5">
        {tab === "downloads" ? (
          downloads.length > 0 ? (
            downloads.map((d) => (
              <DownloadRow
                key={d.id}
                d={d}
                now={now}
                onOpen={onOpenFile}
                onContextMenu={(e, row) => {
                  e.preventDefault();
                  e.stopPropagation();
                  setMenu({ x: e.clientX, y: e.clientY, d: row });
                }}
              />
            ))
          ) : (
            <Empty title="No downloads yet" hint="Files you download from chats show up here." />
          )
        ) : uploads.length > 0 ? (
          uploads.map((u) => <UploadRow key={u.pendingId} u={u} onNavigate={() => onClose(false)} />)
        ) : (
          <Empty title="No uploads this session" hint="Files you send show their progress here." />
        )}
      </div>

      {(cap > 0 || tab === "downloads" || finished) && (
        <div className="flex items-center gap-3 border-t border-border-divider px-3 py-2 text-[12px]">
          {cap > 0 ? (
            <button
              type="button"
              onClick={openSettings}
              className="truncate text-text-muted transition-colors hover:text-text-primary"
              title="Change in Settings → Network"
            >
              {tab === "downloads" ? "↓" : "↑"} Limited to {capLabel(cap)}
            </button>
          ) : (
            <span />
          )}
          <div className="ml-auto flex items-center gap-3">
            {tab === "downloads" && (
              <button
                type="button"
                onClick={() => void window.decibell.downloads.openFolder()}
                className="font-medium text-text-secondary transition-colors hover:text-text-primary"
              >
                Open folder
              </button>
            )}
            {finished && (
              <button
                type="button"
                onClick={() => {
                  if (tab === "downloads") void window.decibell.downloads.clearFinished();
                  else useTransfersStore.getState().clearFinishedUploads();
                }}
                className="font-medium text-text-secondary transition-colors hover:text-text-primary"
              >
                Clear
              </button>
            )}
          </div>
        </div>
      )}

      {menu && <RowMenu menu={menu} onOpenFile={onOpenFile} onDone={() => setMenu(null)} />}
    </div>,
    document.body,
  );
}

function Empty({ title, hint }: { title: string; hint: string }) {
  return (
    <div className="px-6 py-8 text-center">
      <div className="text-[13px] font-medium text-text-secondary">{title}</div>
      <div className="mt-1 text-[12px] text-text-muted">{hint}</div>
    </div>
  );
}

function RowMenu({
  menu,
  onOpenFile,
  onDone,
}: {
  menu: MenuState;
  onOpenFile: (d: DownloadView) => void;
  onDone: () => void;
}) {
  const { d } = menu;
  const api = window.decibell.downloads;
  const canOpen = d.state === "done" && !d.missing;
  const items: Array<{ label: string; run: () => void; show: boolean }> = [
    { label: "Open", run: () => onOpenFile(d), show: canOpen },
    { label: "Show in folder", run: () => void api.showInFolder(d.id), show: canOpen },
    { label: "Remove from list", run: () => void api.remove(d.id), show: d.state !== "active" },
    { label: "Cancel", run: () => void api.cancel(d.id), show: d.state === "active" },
  ];
  const style: React.CSSProperties = {
    left: Math.min(menu.x, window.innerWidth - 188),
    top: Math.min(menu.y, window.innerHeight - 140),
  };
  return (
    <div
      role="menu"
      style={style}
      className="fixed z-[101] min-w-[180px] rounded-md border border-border bg-bg-light p-1 shadow-float animate-[fadeUp_0.12s_ease_both]"
      onClick={(e) => e.stopPropagation()}
    >
      {items
        .filter((i) => i.show)
        .map((i) => (
          <button
            key={i.label}
            type="button"
            role="menuitem"
            onClick={() => {
              onDone();
              i.run();
            }}
            className="flex w-full items-center rounded-sm px-2.5 py-1.5 text-left text-[13px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
          >
            {i.label}
          </button>
        ))}
    </div>
  );
}
