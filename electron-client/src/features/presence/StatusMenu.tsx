// The status picker on the user panel (docs/superpowers/specs/
// 2026-10-11-user-status-design.md). Central saves the choice and tells
// everyone what they may see; Invisible also has native tell every
// connected community to list us offline.

import { useEffect } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { useMenuPosition, type MenuAnchor } from "../../hooks/useMenuPosition";
import { STATUS_LABEL, usePresenceStore, type OwnStatus } from "../../stores/presenceStore";
import { useUiStore } from "../../stores/uiStore";
import { StatusDot } from "../../components/StatusDot";

const OPTIONS: { status: OwnStatus; hint?: string }[] = [
  { status: "online" },
  { status: "away" },
  { status: "dnd", hint: "Incoming calls arrive without ringing" },
  { status: "invisible", hint: "You appear offline but can use Decibell as usual" },
];

/// Optimistic: the menu reflects the pick at once; central applies it and
/// saves it for the next login.
export function chooseStatus(status: OwnStatus) {
  usePresenceStore.getState().setOwnStatus(status);
  invoke("set_status", { status }).catch(console.error);
}

export function awayAfterLabel(minutes: number): string {
  if (minutes <= 0) return "Never";
  if (minutes % 60 === 0) return minutes === 60 ? "1 hour" : `${minutes / 60} hours`;
  return minutes === 1 ? "1 minute" : `${minutes} minutes`;
}

export default function StatusMenu({ anchor, onClose }: { anchor: MenuAnchor; onClose: () => void }) {
  const { ref, style } = useMenuPosition(anchor, { prefer: "above" });
  const ownStatus = usePresenceStore((s) => s.ownStatus);
  const awayAfter = useUiStore((s) => s.awayAfterMinutes);

  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const handleKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    // Next tick, so the click that opened the menu doesn't close it.
    const timer = setTimeout(() => {
      document.addEventListener("mousedown", handleClick);
      document.addEventListener("keydown", handleKey);
    }, 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener("mousedown", handleClick);
      document.removeEventListener("keydown", handleKey);
    };
  }, [onClose, ref]);

  return createPortal(
    <div
      ref={ref}
      role="menu"
      aria-label="Set status"
      className="z-[100] w-60 rounded-lg border border-border bg-bg-secondary p-1 shadow-modal"
      style={style}
    >
      {OPTIONS.map(({ status, hint }) => {
        const selected = ownStatus === status;
        return (
          <button
            key={status}
            type="button"
            role="menuitemradio"
            aria-checked={selected}
            onClick={() => {
              if (!selected) chooseStatus(status);
              onClose();
            }}
            className="flex w-full items-start gap-2.5 rounded-sm px-2.5 py-2 text-left transition-colors hover:bg-surface-hover"
          >
            <StatusDot
              status={status}
              ring="var(--color-bg-secondary)"
              ringWidth={0}
              className="mt-1 h-2.5 w-2.5"
            />
            <span className="min-w-0 flex-1">
              <span className="block text-[13px] font-medium text-text-primary">{STATUS_LABEL[status]}</span>
              {hint && <span className="mt-0.5 block text-[12px] leading-snug text-text-muted">{hint}</span>}
            </span>
            {selected && (
              <svg className="mt-0.5 h-3.5 w-3.5 shrink-0 text-accent" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                <polyline points="20 6 9 17 4 12" />
              </svg>
            )}
          </button>
        );
      })}
      <div className="mx-1.5 my-1 h-px bg-border-divider" />
      <button
        type="button"
        onClick={() => {
          useUiStore.getState().setSettingsTab("privacy");
          useUiStore.getState().openModal("settings");
          onClose();
        }}
        className="w-full rounded-sm px-2.5 py-2 text-left text-[12px] text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
      >
        {awayAfter > 0
          ? `Away after ${awayAfterLabel(awayAfter)} idle or when locked`
          : "Automatic away is off"}
      </button>
    </div>,
    document.body,
  );
}
