import type { ReactNode } from "react";

/// The chrome shared by Settings, Server Settings and Channel Settings: a
/// sidebar of tabs beside a scrolling pane. Below 1024 px (max-lg) the
/// window goes nearly edge to edge and the sidebar folds into a rail of
/// icons with hover labels, so the two-column tabs (Roles, Permissions)
/// keep room to work down to the app's 800 × 600 minimum.

/// The dimmed backdrop. Its padding sets the window's margin.
export const SETTINGS_BACKDROP =
  "fixed inset-0 z-50 flex items-center justify-center px-[max(calc(var(--spacing)*14),5vw)] py-[max(calc(var(--spacing)*12),6vh)] transition-colors duration-300 max-lg:p-4";

/// The window itself.
export const SETTINGS_FRAME =
  "flex h-full max-h-280 w-full max-w-400 overflow-hidden rounded-xl border border-border bg-bg-dark shadow-modal transition-[opacity,transform] duration-300";

/// The scrolling pane beside the sidebar. The gutter is held so a tab
/// that scrolls is as wide as one that doesn't.
export const SETTINGS_PANE = "flex flex-1 flex-col overflow-y-auto scrollbar-thin [scrollbar-gutter:stable]";

/// The pane's body, under its header.
export const SETTINGS_BODY = "flex-1 px-8 pb-7 max-lg:px-6";

/// The sidebar. Above the pane, so a rail label can overlap it.
export function SettingsNav({ children }: { children: ReactNode }) {
  return (
    <nav className="relative z-30 flex w-60 shrink-0 flex-col gap-0.5 border-r border-border-divider bg-bg-darkest px-3 py-6 max-lg:w-16">
      {children}
    </nav>
  );
}

/// The small uppercase label at the top of the sidebar (hidden on the rail).
export function SettingsNavLabel({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <div
      title={title}
      className="mb-2 truncate px-3 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted max-lg:hidden"
    >
      {children}
    </div>
  );
}

/// A tab in the sidebar. On the rail it shows only its icon; the label
/// appears beside it on hover or keyboard focus.
export function SettingsNavItem({
  icon,
  label,
  active = false,
  dot = false,
  danger = false,
  onClick,
}: {
  icon: ReactNode;
  label: string;
  active?: boolean;
  dot?: boolean;
  danger?: boolean;
  onClick: () => void;
}) {
  const tone = danger
    ? "font-normal text-text-muted hover:bg-error/10 hover:text-error"
    : active
      ? "bg-accent-soft font-medium text-text-primary"
      : "font-normal text-text-secondary hover:bg-surface-hover hover:text-text-primary";
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      aria-current={active ? "page" : undefined}
      className={`group relative flex items-center gap-2.5 rounded-md px-3 py-[9px] text-[14px] transition-colors max-lg:h-10 max-lg:justify-center max-lg:px-0 max-lg:py-0 ${tone}`}
    >
      <span className={danger ? "" : active ? "text-accent-bright" : "text-text-muted group-hover:text-text-secondary"}>
        {icon}
      </span>
      <span className="truncate max-lg:hidden">{label}</span>
      {dot && (
        <span
          className="ml-auto h-1.5 w-1.5 shrink-0 rounded-full bg-accent max-lg:absolute max-lg:right-1 max-lg:top-1"
          title="Unsaved changes"
        />
      )}
      <span className="pointer-events-none absolute left-full top-1/2 ml-4 hidden -translate-y-1/2 whitespace-nowrap rounded-md border border-border bg-bg-darkest px-2.5 py-1.5 text-[13px] font-medium text-text-primary opacity-0 shadow-float transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100 max-lg:block">
        {label}
      </span>
    </button>
  );
}

/// Pushes the items after it to the foot of the sidebar, under a divider.
export function SettingsNavFoot({ children }: { children: ReactNode }) {
  return (
    <>
      <div className="flex-1" />
      <div className="mx-1.5 mb-1 h-px bg-border-divider" />
      {children}
    </>
  );
}

/// The pane's title row. On the rail the window has no sidebar heading, so
/// `context` (the server or channel name) shows beside the title instead.
export function SettingsPaneHeader({
  title,
  context,
  onClose,
}: {
  title: string;
  context?: ReactNode;
  onClose: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-4 px-8 pt-7 pb-5 max-lg:px-6">
      <div className="flex min-w-0 items-baseline gap-2.5">
        <h2 className="shrink-0 font-display text-[18px] font-semibold text-text-primary">{title}</h2>
        {context && (
          <span className="hidden min-w-0 truncate text-[14px] text-text-muted max-lg:inline">{context}</span>
        )}
      </div>
      <button
        type="button"
        onClick={onClose}
        aria-label="Close"
        className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>
    </div>
  );
}
