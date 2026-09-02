import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useVoiceStore } from "../../stores/voiceStore";
import StreamAudioAppPicker from "./StreamAudioAppPicker";
import { canPickStreamAudioApps } from "./streamAudioFilter";
import { activeStreamSourceId } from "./streaming/StreamCapture";

/// Icon button that opens the stream-audio app picker as a popover while a
/// stream with audio is live. Renders nothing otherwise, so mount sites can
/// drop it next to their Stop button unconditionally.
export function StreamAudioButton({ className, size = 16 }: { className?: string; size?: number }) {
  const isStreaming = useVoiceStore((s) => s.isStreaming);
  const shareAudio = useVoiceStore((s) => s.streamSettings.shareAudio);
  const [open, setOpen] = useState(false);
  const btnRef = useRef<HTMLButtonElement>(null);

  if (!isStreaming || !shareAudio || !canPickStreamAudioApps()) return null;

  return (
    <>
      <button
        ref={btnRef}
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
        title="Stream audio apps"
        aria-label="Stream audio apps"
        aria-expanded={open}
        className={className}
      >
        <svg
          width={size}
          height={size}
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
          <path d="M15.54 8.46a5 5 0 0 1 0 7.07" />
          <path d="M19.07 4.93a10 10 0 0 1 0 14.14" />
        </svg>
      </button>
      {open && btnRef.current && (
        <StreamAudioPopover anchorEl={btnRef.current} onClose={() => setOpen(false)} />
      )}
    </>
  );
}

/// The picker in a floating card anchored to `anchorEl` (above it when the
/// anchor sits low on the screen, which is where the Stop button lives).
/// Closes on outside click / Escape; clicks on the anchor itself are the
/// toggle's job, not ours.
export function StreamAudioPopover({ anchorEl, onClose }: { anchorEl: HTMLElement; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      const t = e.target as Node;
      if (ref.current && !ref.current.contains(t) && !anchorEl.contains(t)) onClose();
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const timer = setTimeout(() => {
      document.addEventListener("mousedown", handleClick);
      document.addEventListener("keydown", handleKey);
    }, 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [anchorEl, onClose]);

  const width = 300;
  const margin = 8;
  const estHeight = 280;
  const rect = anchorEl.getBoundingClientRect();
  const left = Math.max(margin, Math.min(rect.left, window.innerWidth - width - margin));
  const placeAbove = rect.bottom + estHeight + margin > window.innerHeight && rect.top > estHeight;
  const style: React.CSSProperties = placeAbove
    ? { left, bottom: window.innerHeight - rect.top + margin, width }
    : { left, top: rect.bottom + margin, width };

  return createPortal(
    <div
      ref={ref}
      style={style}
      role="dialog"
      aria-label="Stream audio apps"
      className="fixed z-[100] flex flex-col gap-2 rounded-lg border border-border bg-bg-secondary p-3 shadow-modal"
      // The card / row that hosts the button may have its own click and key
      // handlers (CallStage's StreamCard opens the preview); React events
      // bubble through portals, so stop them here.
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => e.stopPropagation()}
    >
      <div className="text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
        Stream audio
      </div>
      <StreamAudioAppPicker dense sourceId={activeStreamSourceId()} />
    </div>,
    document.body,
  );
}
