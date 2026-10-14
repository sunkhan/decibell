import { useEffect, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { useMenuPosition, type MenuAnchor } from "../../../hooks/useMenuPosition";
import type { ServerRole } from "../../../types";
import { roleColor } from "./helpers";

/// Building blocks shared by the settings tabs: one stroke icon set, the
/// toolbar search field, an anchored popover, menu rows, the role chip
/// and the save bar.

const PATHS = {
  search: <><circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" /></>,
  x: <path d="M18 6 6 18M6 6l12 12" />,
  plus: <path d="M12 5v14M5 12h14" />,
  check: <path d="M20 6 9 17l-5-5" />,
  more: (
    <>
      <circle cx="5" cy="12" r="1.75" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1.75" fill="currentColor" stroke="none" />
      <circle cx="19" cy="12" r="1.75" fill="currentColor" stroke="none" />
    </>
  ),
  grip: (
    <>
      {[6, 12, 18].map((y) => (
        <g key={y}>
          <circle cx="9" cy={y} r="1.6" fill="currentColor" stroke="none" />
          <circle cx="15" cy={y} r="1.6" fill="currentColor" stroke="none" />
        </g>
      ))}
    </>
  ),
  lock: <><rect x="5" y="11" width="14" height="10" rx="2" /><path d="M8 11V7a4 4 0 0 1 8 0v4" /></>,
  pencil: <><path d="M12 20h9" /><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" /></>,
  tag: <><path d="M20.59 13.41 13.42 20.58a2 2 0 0 1-2.83 0L2 12V2h10l8.59 8.59a2 2 0 0 1 0 2.82Z" /><circle cx="7" cy="7" r="1.5" /></>,
  clock: <><circle cx="12" cy="12" r="9" /><path d="M12 7v5l3 2" /></>,
  userX: <><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" /><circle cx="9" cy="7" r="4" /><path d="m17 8 5 5M22 8l-5 5" /></>,
  userCheck: <><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" /><circle cx="9" cy="7" r="4" /><path d="m16 11 2 2 4-4" /></>,
  userPlus: <><path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2" /><circle cx="9" cy="7" r="4" /><path d="M19 8v6M16 11h6" /></>,
  users: <><path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" /><circle cx="9" cy="7" r="4" /><path d="M23 21v-2a4 4 0 0 0-3-3.87M16 3.13a4 4 0 0 1 0 7.75" /></>,
  ban: <><circle cx="12" cy="12" r="9" /><path d="m5.6 5.6 12.8 12.8" /></>,
  shield: <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />,
  shieldPlus: <><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" /><path d="M12 8v6M9 11h6" /></>,
  shieldX: <><path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" /><path d="m9.5 8.5 5 5M14.5 8.5l-5 5" /></>,
  hash: <path d="M4 9h16M4 15h16M10 3 8 21M16 3l-2 18" />,
  sliders: <path d="M4 21v-7M4 10V3M12 21v-9M12 8V3M20 21v-5M20 12V3M1 14h6M9 8h6M17 16h6" />,
  trash: <path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />,
  eraser: <><path d="m7 21-4.3-4.3a1 1 0 0 1 0-1.4l10-10a1 1 0 0 1 1.4 0l5.6 5.6a1 1 0 0 1 0 1.4L13 21" /><path d="M22 21H7M5 11l9 9" /></>,
  refresh: <><path d="M21 12a9 9 0 0 1-15.5 6.2L3 16M3 12a9 9 0 0 1 15.5-6.2L21 8" /><path d="M21 3v5h-5M3 21v-5h5" /></>,
  messageX: <><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" /><path d="m9.5 7.5 5 5M14.5 7.5l-5 5" /></>,
  link: <><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" /><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" /></>,
  linkOff: <><path d="M9 17H7A5 5 0 0 1 7 7M15 7h2a5 5 0 0 1 4 8" /><path d="M8 12h4M2 2l20 20" /></>,
  gear: <><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" /></>,
  database: <><ellipse cx="12" cy="5" rx="9" ry="3" /><path d="M3 5v14a9 3 0 0 0 18 0V5" /><path d="M3 12a9 3 0 0 0 18 0" /></>,
  crown: <path d="m2 7 5 5 5-8 5 8 5-5-2 12H4z" />,
  micOff: <><path d="m2 2 20 20" /><path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V5a3 3 0 0 0-5.94-.6" /><path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2a7 7 0 0 1-.11 1.23M12 19v3" /></>,
  mic: <><rect x="9" y="2" width="6" height="12" rx="3" /><path d="M19 10v2a7 7 0 0 1-14 0v-2M12 19v3" /></>,
  headphones: <><path d="M3 18v-6a9 9 0 0 1 18 0v6" /><path d="M21 19a2 2 0 0 1-2 2h-1v-6h3zM3 19a2 2 0 0 0 2 2h1v-6H3z" /></>,
  move: <path d="M5 12h14M13 6l6 6-6 6" />,
  logout: <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4M16 17l5-5-5-5M21 12H9" />,
  alert: <><path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" /><path d="M12 9v4M12 17h.01" /></>,
  palette: <><circle cx="13.5" cy="6.5" r="1" fill="currentColor" /><circle cx="17.5" cy="10.5" r="1" fill="currentColor" /><circle cx="8.5" cy="7.5" r="1" fill="currentColor" /><circle cx="6.5" cy="12.5" r="1" fill="currentColor" /><path d="M12 2a10 10 0 0 0 0 20 2 2 0 0 0 2-2v-.5a2 2 0 0 1 2-2h1.5A4.5 4.5 0 0 0 22 13 11 11 0 0 0 12 2z" /></>,
  key: <><circle cx="7.5" cy="15.5" r="4.5" /><path d="m10.7 12.3 9.3-9.3M17 6l3 3M14 9l2 2" /></>,
  file: <><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" /><path d="M14 2v6h6M16 13H8M16 17H8" /></>,
  slash: <path d="M16 5 8 19" />,
  speaker: <><path d="M11 5 6 9H2v6h4l5 4z" /><path d="M15.54 8.46a5 5 0 0 1 0 7.07M19.07 4.93a10 10 0 0 1 0 14.14" /></>,
  folder: <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />,
  lockOpen: <><rect x="5" y="11" width="14" height="10" rx="2" /><path d="M8 11V7a4 4 0 0 1 7.75-1.4" /></>,
  message: <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />,
  image: <><rect x="3" y="3" width="18" height="18" rx="2" /><circle cx="8.5" cy="8.5" r="1.5" /><path d="m21 15-5-5L5 21" /></>,
  film: <><rect x="2" y="4" width="20" height="16" rx="2" /><path d="M7 4v16M17 4v16M2 9h5M2 15h5M17 9h5M17 15h5" /></>,
  music: <><path d="M9 18V5l12-2v13" /><circle cx="6" cy="18" r="3" /><circle cx="18" cy="16" r="3" /></>,
  hourglass: <path d="M6 2h12M6 22h12M7 2v4.5a5 5 0 0 0 2 4L12 12l3-1.5a5 5 0 0 0 2-4V2M7 22v-4.5a5 5 0 0 1 2-4L12 12l3 1.5a5 5 0 0 1 2 4V22" />,
  waves: <path d="M2 12h2M6 8v8M10 5v14M14 9v6M18 7v10M22 12h-2" />,
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({
  name,
  size = 16,
  strokeWidth = 2,
  className,
}: {
  name: IconName;
  size?: number;
  strokeWidth?: number;
  className?: string;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      className={`shrink-0 ${className ?? ""}`}
    >
      {PATHS[name]}
    </svg>
  );
}

/// Toolbar search: same 40px height, fill and focus ring as a md Select
/// so the two sit flush in one row.
export function SearchField({
  value,
  onChange,
  placeholder,
  className = "",
  autoFocus,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder: string;
  className?: string;
  autoFocus?: boolean;
}) {
  return (
    <div className={`relative flex h-10 items-center ${className}`}>
      <Icon name="search" size={15} className="pointer-events-none absolute left-3 text-text-muted" />
      <input
        value={value}
        autoFocus={autoFocus}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          // First Escape clears the field; the modal only closes on an
          // empty one.
          if (e.key === "Escape" && value) {
            e.stopPropagation();
            onChange("");
          }
        }}
        placeholder={placeholder}
        spellCheck={false}
        className="h-full w-full min-w-0 rounded-md border border-border bg-bg-lighter pl-9 pr-8 text-[13px] text-text-primary outline-none transition-all placeholder:text-text-muted hover:border-accent/40 focus:border-accent focus:shadow-ring"
      />
      {value && (
        <button
          type="button"
          onClick={() => onChange("")}
          aria-label="Clear search"
          className="absolute right-2 flex h-6 w-6 items-center justify-center rounded-sm text-text-muted hover:bg-surface-hover hover:text-text-primary"
        >
          <Icon name="x" size={12} strokeWidth={2.5} />
        </button>
      )}
    </div>
  );
}

/// Anchor a popover under (or above) an element's bottom-left corner.
export function anchorBelow(el: HTMLElement, alignRight = false): MenuAnchor {
  const r = el.getBoundingClientRect();
  return { x: Math.round(alignRight ? r.right : r.left), y: Math.round(r.bottom + 4) };
}

/// A fixed, portaled popover kept inside the viewport (useMenuPosition).
/// Outside mousedown and Escape close it; Escape is captured so it never
/// reaches the settings modal's own Escape-to-close. `ignore` is the
/// trigger, so pressing it again toggles instead of close-then-reopen.
export function Popover({
  anchor,
  onClose,
  ignore,
  className = "",
  children,
}: {
  anchor: MenuAnchor;
  onClose: () => void;
  ignore?: RefObject<HTMLElement | null>;
  className?: string;
  children: ReactNode;
}) {
  const { ref, style } = useMenuPosition(anchor);
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      const t = e.target as Node;
      if (ref.current?.contains(t) || ignore?.current?.contains(t)) return;
      onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      onClose();
    };
    document.addEventListener("mousedown", onDown, true);
    window.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [onClose, ref, ignore]);
  // Menus take focus on open (unless a field inside already did), so the
  // arrow keys work straight away.
  useEffect(() => {
    const el = ref.current;
    if (el && !el.contains(document.activeElement)) items(el)[0]?.focus();
  }, [ref]);
  return createPortal(
    <div
      ref={ref}
      style={style}
      onKeyDown={(e) => {
        if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
        const list = items(e.currentTarget);
        if (!list.length) return;
        e.preventDefault();
        const at = list.indexOf(document.activeElement as HTMLElement);
        const step = e.key === "ArrowDown" ? 1 : -1;
        list[(at + step + list.length) % list.length].focus();
      }}
      onClick={(e) => e.stopPropagation()}
      className={`z-[100] animate-[dropIn_0.15s_ease] rounded-lg border border-border bg-bg-light shadow-float ${className}`}
    >
      {children}
    </div>,
    document.body,
  );
}

const items = (root: HTMLElement) =>
  Array.from(root.querySelectorAll<HTMLElement>("[role^=menuitem]:not(:disabled)"));

export function MenuItem({
  icon,
  label,
  onClick,
  danger,
  disabled,
}: {
  icon: IconName;
  label: string;
  onClick: () => void;
  danger?: boolean;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      disabled={disabled}
      onClick={onClick}
      className={`flex w-full items-center gap-2.5 rounded-md px-2.5 py-2 text-left text-[13px] font-medium outline-none focus-visible:bg-surface-hover disabled:opacity-40 ${
        danger
          ? "text-error hover:bg-error/10"
          : "text-text-primary hover:bg-surface-hover"
      }`}
    >
      <Icon name={icon} size={15} className={danger ? "" : "text-text-muted"} />
      <span className="min-w-0 flex-1 truncate">{label}</span>
    </button>
  );
}

export function MenuDivider() {
  return <div className="mx-2.5 my-1 h-px bg-border-divider" />;
}

/// A role as a chip: colour dot + name. With `onRemove`, hovering swaps
/// the dot for a ✕ in the same 12px slot (no width change on hover).
export function RoleChip({ role, onRemove }: { role: ServerRole; onRemove?: () => void }) {
  const name = role.isDefault ? "@everyone" : role.name;
  return (
    <span className="group/chip inline-flex h-6 min-w-0 max-w-40 items-center gap-1.5 rounded-sm border border-border-divider bg-bg-light pl-1.5 pr-2 text-[12px] font-medium text-text-secondary">
      {onRemove ? (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            onRemove();
          }}
          aria-label={`Remove ${name}`}
          title={`Remove ${name}`}
          className="flex h-3 w-3 shrink-0 items-center justify-center"
        >
          <span
            className="h-2.5 w-2.5 rounded-full group-hover/chip:hidden"
            style={{ backgroundColor: roleColor(role.color) }}
          />
          <Icon name="x" size={12} strokeWidth={3} className="hidden text-text-primary group-hover/chip:block" />
        </button>
      ) : (
        <span className="flex h-3 w-3 shrink-0 items-center justify-center">
          <span className="h-2.5 w-2.5 rounded-full" style={{ backgroundColor: roleColor(role.color) }} />
        </span>
      )}
      <span className="truncate">{name}</span>
    </span>
  );
}

/// The uppercase group label (ONLINE / OFFLINE idiom).
export function SectionLabel({ children, className = "" }: { children: ReactNode; className?: string }) {
  return (
    <div className={`font-section text-section font-medium uppercase leading-none tracking-section text-text-muted ${className}`}>
      {children}
    </div>
  );
}

/// The floating save bar under a settings draft, shown while it has
/// unsaved changes. `nudge` counts refused leaves (a role switch, closing
/// the window): each one shakes the bar and swaps in `nudgeMessage`.
/// Render it wherever the draft can be edited, shown or not: its space is
/// always held, so the bar arriving never makes the tab taller (a new
/// scrollbar would narrow everything above it).
export function SaveBar({
  show,
  nudge,
  saving,
  onReset,
  onSave,
  message = "You have unsaved changes.",
  nudgeMessage = "Save or reset your changes first.",
  resetLabel = "Reset",
  saveLabel = "Save changes",
  saveDisabled = false,
}: {
  show: boolean;
  nudge: number;
  saving: boolean;
  onReset: () => void;
  onSave: () => void;
  message?: string;
  nudgeMessage?: string;
  resetLabel?: string;
  saveLabel?: string;
  saveDisabled?: boolean;
}) {
  return (
    <div className="pointer-events-none sticky bottom-4 z-20 mt-8 flex h-14 justify-center">
      {show && (
        <div
          key={nudge}
          className={`pointer-events-auto flex w-full items-center gap-2 rounded-lg border border-border bg-bg-darkest pl-4 pr-2.5 shadow-float ${
            nudge ? "animate-[barNudge_0.36s_ease]" : "animate-[fadeUp_0.2s_ease]"
          }`}
        >
          <span className="min-w-0 flex-1 truncate text-[13px] font-medium text-text-primary">
            {nudge ? nudgeMessage : message}
          </span>
          <button
            type="button"
            onClick={onReset}
            disabled={saving}
            className="rounded-sm px-4 py-2 text-[13px] font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-50"
          >
            {resetLabel}
          </button>
          <button
            type="button"
            onClick={onSave}
            disabled={saving || saveDisabled}
            className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
          >
            {saving ? "Saving…" : saveLabel}
          </button>
        </div>
      )}
    </div>
  );
}

export function formatDate(epoch: number): string {
  if (!epoch) return "—";
  return new Date(epoch * 1000).toLocaleDateString(undefined, {
    month: "short",
    day: "numeric",
    year: "numeric",
  });
}

/// "2h 59m" / "4d 3h" / "45s" until `epoch`.
export function formatRemaining(epoch: number): string {
  let s = Math.max(0, Math.round(epoch - Date.now() / 1000));
  const d = Math.floor(s / 86400);
  s -= d * 86400;
  const h = Math.floor(s / 3600);
  s -= h * 3600;
  const m = Math.floor(s / 60);
  if (d) return `${d}d ${h}h`;
  if (h) return `${h}h ${m}m`;
  if (m) return `${m}m`;
  return `${s}s`;
}
