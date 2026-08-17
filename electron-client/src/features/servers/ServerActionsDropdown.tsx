interface ServerActionsDropdownProps {
  /// MANAGE_INVITES (or owner) — drives the Invites entry.
  canInvite: boolean;
  onInvites: () => void;
  onDisconnect: () => void;
  /// Opens the unified server-settings screen (Overview / Members /
  /// Roles / Bans). Visible to everyone — the tabs inside gate
  /// themselves by permission.
  onServerSettings: () => void;
}

/// Server-name dropdown. Channel settings moved out of here to the
/// per-channel-row gear icons in the sidebar (Discord-style).
export default function ServerActionsDropdown({
  canInvite,
  onInvites,
  onDisconnect,
  onServerSettings,
}: ServerActionsDropdownProps) {
  return (
    <div className="absolute left-2 right-2 top-full z-30 mt-1.5 animate-[dropIn_0.18s_ease] rounded-md border border-border bg-bg-light p-[5px] shadow-float">
      <button
        onClick={onServerSettings}
        className="group flex w-full cursor-pointer items-center gap-2.5 rounded-sm px-2.5 py-[9px] text-[13px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 text-text-muted transition-colors group-hover:text-text-secondary">
          <rect x="3" y="3" width="18" height="18" rx="2" />
          <path d="M3 9h18M9 3v18" />
        </svg>
        Server Settings
      </button>
      {canInvite && (
        <button
          onClick={onInvites}
          className="group flex w-full cursor-pointer items-center gap-2.5 rounded-sm px-2.5 py-[9px] text-[13px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 text-text-muted transition-colors group-hover:text-text-secondary">
            <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" />
            <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" />
          </svg>
          Invites
        </button>
      )}
      <div className="mx-1.5 my-1 h-px bg-border-divider" />
      <button
        onClick={onDisconnect}
        className="flex w-full cursor-pointer items-center gap-2.5 rounded-sm px-2.5 py-[9px] text-[13px] text-text-muted transition-colors hover:bg-error/10 hover:text-error"
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 transition-colors">
          <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
          <polyline points="16 17 21 12 16 7" />
          <line x1="21" y1="12" x2="9" y2="12" />
        </svg>
        Leave Server
      </button>
    </div>
  );
}
