interface ServerActionsDropdownProps {
  isOwner: boolean;
  onMembers: () => void;
  onInvites: () => void;
  onDisconnect: () => void;
}

export default function ServerActionsDropdown({
  isOwner,
  onMembers,
  onInvites,
  onDisconnect,
}: ServerActionsDropdownProps) {
  return (
    <div className="absolute left-2 right-2 top-full z-30 mt-1.5 animate-[dropIn_0.18s_ease] rounded-[10px] border border-border bg-bg-light p-[5px] shadow-[0_8px_32px_rgba(0,0,0,0.45),0_0_0_1px_rgba(255,255,255,0.02)]">
      <button
        onClick={onMembers}
        className="group flex w-full items-center gap-2.5 rounded-md px-2.5 py-[9px] text-[13px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 text-text-muted transition-colors group-hover:text-text-secondary">
          <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" />
          <circle cx="9" cy="7" r="4" />
          <path d="M23 21v-2a4 4 0 0 0-3-3.87" />
          <path d="M16 3.13a4 4 0 0 1 0 7.75" />
        </svg>
        Members
      </button>
      {isOwner && (
        <button
          onClick={onInvites}
          className="group flex w-full items-center gap-2.5 rounded-md px-2.5 py-[9px] text-[13px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
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
        className="flex w-full items-center gap-2.5 rounded-md px-2.5 py-[9px] text-[13px] text-text-muted transition-colors hover:bg-error/10 hover:text-error"
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 transition-colors">
          <path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4" />
          <polyline points="16 17 21 12 16 7" />
          <line x1="21" y1="12" x2="9" y2="12" />
        </svg>
        Disconnect
      </button>
    </div>
  );
}
