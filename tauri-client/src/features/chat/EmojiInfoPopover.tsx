import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
} from "react";
import { createPortal } from "react-dom";
import Twemoji from "../../components/emoji/Twemoji";

interface EmojiInfoPopoverProps {
  anchor: HTMLElement;
  emoji: string;
  shortcode: string | null;
  source: string;
  onClose: () => void;
}

export default function EmojiInfoPopover({
  anchor,
  emoji,
  shortcode,
  source,
  onClose,
}: EmojiInfoPopoverProps) {
  const popoverRef = useRef<HTMLDivElement>(null);
  const [style, setStyle] = useState<CSSProperties>({
    position: "fixed",
    left: 0,
    top: 0,
    opacity: 0,
  });

  // Position to the right of the anchor, flipping to the left if there isn't
  // room. Vertically centered on the emoji, clamped to the viewport.
  useLayoutEffect(() => {
    const pop = popoverRef.current;
    if (!pop) return;
    const anchorRect = anchor.getBoundingClientRect();
    const popRect = pop.getBoundingClientRect();
    const gap = 8;
    let left = anchorRect.right + gap;
    if (left + popRect.width > window.innerWidth - 8) {
      left = anchorRect.left - popRect.width - gap;
    }
    left = Math.max(8, left);
    let top =
      anchorRect.top + anchorRect.height / 2 - popRect.height / 2;
    top = Math.max(
      8,
      Math.min(window.innerHeight - popRect.height - 8, top)
    );
    setStyle({ position: "fixed", left, top, opacity: 1 });
  }, [anchor]);

  useEffect(() => {
    const handleMouseDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (popoverRef.current?.contains(target)) return;
      if (anchor.contains(target)) return;
      onClose();
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const handleScroll = () => onClose();
    document.addEventListener("mousedown", handleMouseDown);
    document.addEventListener("keydown", handleKey);
    window.addEventListener("scroll", handleScroll, true);
    return () => {
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleKey);
      window.removeEventListener("scroll", handleScroll, true);
    };
  }, [anchor, onClose]);

  return createPortal(
    <div
      ref={popoverRef}
      style={{
        ...style,
        animation: style.opacity ? "tooltipIn 0.12s ease both" : undefined,
      }}
      className="z-50 min-w-[180px] overflow-hidden rounded-xl border border-border bg-bg-light shadow-[0_8px_32px_rgba(0,0,0,0.45),0_0_0_1px_rgba(255,255,255,0.02)]"
    >
      <div className="flex items-center gap-3 border-b border-border-divider px-4 py-3">
        <Twemoji emoji={emoji} size={36} />
        <span className="text-[13px] font-medium text-text-primary">
          {shortcode ? `:${shortcode}:` : emoji}
        </span>
      </div>
      <div className="px-4 py-2 text-[11px] text-text-muted">{source}</div>
    </div>,
    document.body
  );
}
