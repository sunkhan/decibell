import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import ConfirmModal from "../../components/ConfirmModal";
import { useTransfersStore } from "../../stores/transfersStore";
import { toast } from "../../stores/toastStore";
import type { DownloadView } from "../../types";
import TransfersPanel from "./TransfersPanel";
import { isRiskyFile } from "./format";

const RING_R = 10;
const RING_C = 2 * Math.PI * RING_R;

/// The Transfers button in the title bar, left of the window controls.
/// A ring around the glyph fills with the combined progress of what's
/// running (muted when everything is paused); a dot marks something
/// that finished (accent) or failed (error) while the panel was shut;
/// the arrow drops each time a download starts. Ctrl/Cmd+J toggles.
function TransfersButton() {
  const btnRef = useRef<HTMLButtonElement | null>(null);
  // State, not just the ref: the panel needs the anchor on the render
  // that opens it, even when that's the button's first.
  const [anchor, setAnchor] = useState<HTMLButtonElement | null>(null);
  const setBtn = useCallback((el: HTMLButtonElement | null) => {
    btnRef.current = el;
    setAnchor(el);
  }, []);
  const panelOpen = useTransfersStore((s) => s.panelOpen);
  const attention = useTransfersStore((s) => s.attention);
  const startPulse = useTransfersStore((s) => s.startPulse);
  const downloads = useTransfersStore((s) => s.downloads);
  const uploads = useTransfersStore((s) => s.uploads);
  const [risky, setRisky] = useState<DownloadView | null>(null);

  const live = useMemo(() => {
    let active = 0;
    let paused = 0;
    let done = 0;
    let total = 0;
    const add = (running: boolean, isPaused: boolean, got: number, size: number) => {
      if (!running && !isPaused) return;
      if (running) active += 1;
      else paused += 1;
      if (size > 0) {
        done += got;
        total += size;
      }
    };
    for (const d of downloads) add(d.state === "active", d.state === "paused", d.receivedBytes, d.totalBytes);
    const downloadsRunning = active + paused;
    for (const u of uploads) add(u.state === "uploading", u.state === "paused", u.transferredBytes, u.totalBytes);
    return {
      active,
      paused,
      uploads: active + paused - downloadsRunning,
      fraction: total > 0 ? done / total : 0,
    };
  }, [downloads, uploads]);

  // Taskbar / dock progress, and how many uploads closing the window
  // would lose (main asks before it lets that happen). Progress ticks
  // arrive several times a second; send at most four.
  const sentAt = useRef(0);
  useEffect(() => {
    const send = () => {
      sentAt.current = performance.now();
      const running = live.active + live.paused;
      window.decibell.window
        .setTransferProgress({
          fraction: running > 0 ? live.fraction : -1,
          paused: live.active === 0 && live.paused > 0,
          uploads: live.uploads,
        })
        .catch(() => {});
    };
    const wait = 250 - (performance.now() - sentAt.current);
    if (wait <= 0) {
      send();
      return;
    }
    const t = window.setTimeout(send, wait);
    return () => window.clearTimeout(t);
  }, [live]);

  const close = useCallback((refocus: boolean) => {
    useTransfersStore.getState().closePanel();
    if (refocus) btnRef.current?.focus();
  }, []);

  const toggle = useCallback(() => {
    const s = useTransfersStore.getState();
    if (s.panelOpen) s.closePanel();
    else s.openPanel();
  }, []);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "j") {
        e.preventDefault();
        toggle();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [toggle]);

  const doOpen = useCallback(async (d: DownloadView) => {
    const res = await window.decibell.downloads.open(d.id);
    if (res?.error) toast.error("Couldn't open the file", res.error);
  }, []);
  const openFile = useCallback(
    (d: DownloadView) => {
      if (isRiskyFile(d.filename)) setRisky(d);
      else void doOpen(d);
    },
    [doOpen],
  );

  const running = live.active + live.paused;
  const label =
    running > 0 ? `Transfers · ${running} ${live.active > 0 ? "active" : "paused"}` : "Transfers";
  const ringClass = live.active > 0 ? "text-accent" : "text-text-muted";

  return (
    <>
      <button
        ref={setBtn}
        type="button"
        onClick={toggle}
        aria-label={label}
        title={`${label} (Ctrl+J)`}
        aria-haspopup="dialog"
        aria-expanded={panelOpen}
        className={`relative flex h-full w-11 items-center justify-center transition-colors hover:bg-surface-active hover:text-text-primary ${
          panelOpen ? "bg-surface-active text-text-primary" : "text-text-secondary"
        }`}
      >
        <span className="relative flex h-6 w-6 items-center justify-center">
          {running > 0 && (
            <svg className={`absolute inset-0 -rotate-90 ${ringClass}`} viewBox="0 0 24 24" aria-hidden>
              <circle cx="12" cy="12" r={RING_R} fill="none" stroke="currentColor" strokeOpacity="0.25" strokeWidth="1.5" />
              <circle
                cx="12"
                cy="12"
                r={RING_R}
                fill="none"
                stroke="currentColor"
                strokeWidth="1.5"
                strokeLinecap="round"
                strokeDasharray={`${RING_C * live.fraction} ${RING_C}`}
                className="transition-[stroke-dasharray] duration-300"
              />
            </svg>
          )}
          <svg width="14" height="14" viewBox="0 0 12 12" fill="none" stroke="currentColor" strokeWidth="1.1" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
            <path d="M2 8.5V10h8V8.5" />
            <g
              key={startPulse}
              className={startPulse > 0 ? "animate-[dropIn_0.35s_ease] motion-reduce:animate-none" : undefined}
            >
              <path d="M6 1.8v5.6" />
              <path d="M3.8 5.3 6 7.5l2.2-2.2" />
            </g>
          </svg>
        </span>
        {attention !== "none" && !panelOpen && (
          <span
            aria-hidden
            className={`absolute right-2.5 top-1.5 h-1.5 w-1.5 rounded-full ${
              attention === "failed" ? "bg-error" : "bg-accent"
            }`}
          />
        )}
      </button>
      {panelOpen && anchor && <TransfersPanel anchorEl={anchor} onClose={close} onOpenFile={openFile} />}
      <ConfirmModal
        open={risky !== null}
        title="Open this file?"
        confirmLabel="Open anyway"
        onConfirm={() => {
          const d = risky;
          setRisky(null);
          if (d) void doOpen(d);
        }}
        onCancel={() => setRisky(null)}
      >
        <span className="font-semibold text-text-primary">{risky?.filename}</span> can run programs on
        your computer. Only open it if you trust{" "}
        {risky?.context.sender ? (
          <span className="font-semibold text-text-primary">{risky.context.sender}</span>
        ) : (
          "whoever sent it"
        )}
        .
      </ConfirmModal>
    </>
  );
}

export default memo(TransfersButton);
